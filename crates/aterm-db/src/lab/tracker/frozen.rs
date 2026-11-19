// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE FROZEN EXIT (docs/DESIGN-lab-tracker-2026-11-14.md §4.3, §7 finding 21; package TK-15):
//! once a package is claimed, its `exit` and `outcome` change only by a `change` row someone
//! other than its claimants approved, and a wrong `done` is withdrawn the same way.
//!
//! Like the state rule ([`super::state`]), this is judged where the package is read, from the
//! records that wrote it, so every machine reads the same answer and a write made on one Mac
//! that the guard there could not see the history of is held to the rule too (an import, a
//! bridge's record, two Macs that did not hear each other):
//!
//! - **An edit counts while no claim has seen the package without it.** A write of a package's
//!   `exit` or `outcome` (an [`Edit`]) counts freely only where every write of the package's
//!   state that counts and freezes it ([`super::state::Settled::freezes`]: each to `running`, a
//!   claim, or to `done`) observed it: it was made before the package was claimed, and the
//!   claimant claimed the test it made. An edit made after such a write, at the same time as
//!   one, or in the claim's own record is frozen; one made in the record that states the
//!   package's first state (a birth, an import of a package already running or done) is what
//!   that record states, and counts. The rule reads the package's history, not its state now:
//!   a claimant who releases the package, edits its exit and claims it again is still held to
//!   the exit first claimed.
//! - **A frozen edit counts only where it lands an approved change** ([`Edits::lands`]): a
//!   `change` row for that package and column asking for exactly the value written, whose
//!   `before` is the digest ([`digest`]) of the value the edit replaces (of null, or none,
//!   where it held none), and which the edit's record observed approved. An approval is a
//!   record that writes the change's `approved_by` ([`Approval`]); it counts where the
//!   principal it names is the record's own (one approves as oneself), neither is anyone who
//!   ever wrote the package's claim (its `by`, or that record's own principal), and it saw the
//!   value it lets replaced (so an approval given once is never reused for a value written
//!   after it). The latest writes of `approved_by` the edit saw must all approve: one that
//!   clears it withdraws the approval. A change's package, column, value and `before` are fixed
//!   at its birth, and retiring the row withdraws no edit it let through: what landed stays
//!   landed on every later read.
//! - **An edit that does not count changes nothing.** The package reads as it did before it
//!   ([`Frozen::over`]), and each such edit no later edit of its column saw is a finding
//!   (`frozen-edit`) until someone who has seen it states the column again: stating the value
//!   that stands (null, where none does) is no change, frozen or not.
//! - **A wrong done is withdrawn only by an approved change.** No write of a state leaves
//!   `done` ([`super::state`]) unless it lands an approved change of the package's `state`
//!   asking for the state it writes, with `before` the digest of `"done"`, by the same test:
//!   the approval saw the done it withdraws. Credit for done goes with it.
//!
//! What `claim` records of the exit (its digest) follows from the rule: the exit that stands
//! for a claimed package is one every claim saw, or one an approved change let through, so a
//! `done` is always judged against the test its claimant claimed. Who may approve which change
//! by role (the lane's coordinator; the owner for a loosening) is not judged here: the rule
//! holds the one the design's finding 21 names, a claimant approving their own change.

use super::checks::Finding;
use super::state::{CLAIM, Settled, Write};
use super::tables::{CHANGE, FROZEN, PACKAGE};
use crate::database::Change;
use crate::said::Said;
use crate::typed::{
    cell::{self, CellKey, CellValue, Cells, Projection},
    values::{DIGEST_PREFIX, Principal},
};
use astream_wire::sync::Digest;
use aterm_codec::hex;
use aterm_digest::Sha256;
use aterm_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// The change column that names which package column a change asks for.
const COLUMN: &str = "column";
/// The change column that approves it.
const APPROVED_BY: &str = "approved_by";

/// The digest a claim records of the exit it claimed and a change of the value it replaces
/// (`claim.exit`, `change.before`): `sha256:` and the SHA-256 of the value's JSON, whose
/// object members are in key order, so every machine spells one value alike.
#[must_use]
pub fn digest(value: &Value) -> String {
    let mut text = String::from(DIGEST_PREFIX);
    hex::push_lower(&mut text, &Sha256::digest(value.to_string().as_bytes()));
    text
}

/// One write of a package's frozen column, as its record made it.
#[derive(Clone, Debug, PartialEq)]
pub struct Edit {
    /// The record that wrote it.
    pub operation: Digest,
    /// The column: `exit` or `outcome`.
    pub column: &'static str,
    /// The value it wrote; null where it restored the cell's absence or wrote what no reader
    /// decodes.
    pub value: Value,
    /// What its record says of who made it, why and on what evidence.
    pub said: Option<Said>,
    /// The package's state writes and edits, and every approval, its record observed.
    saw: BTreeSet<Digest>,
    /// The package's state writes whose record observed it.
    heard_by: BTreeSet<Digest>,
}

/// One write of a change's `approved_by`, as its record made it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Approval {
    /// The record that wrote it.
    pub operation: Digest,
    /// The change row's key.
    pub change: String,
    /// The principal it names; none where it cleared the approval.
    pub approver: Option<Principal>,
    /// What its record says of who made it.
    pub said: Option<Said>,
    /// Every state write, edit and other approval its record observed.
    saw: BTreeSet<Digest>,
}

impl Approval {
    /// Whether it approves, for a package `claimants` claimed: it names a principal, that
    /// principal is its record's own, and neither it nor the record's principal is any of the
    /// claimants.
    fn approves(&self, claimants: &[Principal]) -> bool {
        let (Some(approver), Some(said)) = (&self.approver, &self.said) else {
            return false;
        };
        same(approver, &said.principal)
            && !claimants
                .iter()
                .any(|by| same(by, approver) || same(by, &said.principal))
    }
}

/// Whether two principals name one writer: every member both state is the same, and they state
/// at least one of the members that name a writer (its session, its program, what it said) in
/// common; a machine or an account alone names no writer. A principal that leaves out what the
/// other names is taken to be it, so an approver who does not say its session is held to be
/// the claimant it could be.
fn same(a: &Principal, b: &Principal) -> bool {
    let named = [
        (&a.session, &b.session),
        (&a.program, &b.program),
        (&a.said, &b.said),
    ];
    let also = [(&a.machine, &b.machine), (&a.account, &b.account)];
    let agree = |pairs: &[(&Option<String>, &Option<String>)]| {
        pairs
            .iter()
            .filter_map(|(a, b)| a.as_ref().zip(b.as_ref()))
            .all(|(a, b)| a == b)
    };
    named.iter().any(|(a, b)| a.is_some() && b.is_some()) && agree(&named) && agree(&also)
}

/// What a change row asks for: the package, the column, the value and the digest of the value
/// it replaces, each fixed at its birth.
#[derive(Clone, Debug, PartialEq)]
pub struct Asked {
    pub package: String,
    pub column: String,
    pub value: Value,
    pub before: Option<String>,
}

/// Every change row the table holds, by its key, a retired one included: retiring a change
/// withdraws no edit it let through (clearing its `approved_by` withdraws the approval).
#[must_use]
pub fn asked(cells: &Cells) -> BTreeMap<String, Asked> {
    let text =
        |row: &cell::Row, column: &str| row.get(column).and_then(Value::as_str).map(str::to_owned);
    cell::project(&CHANGE, cells)
        .rows
        .iter()
        .filter_map(|(pk, row)| {
            let column = text(row, COLUMN)?;
            Some((
                pk.clone(),
                Asked {
                    package: text(row, "package")?,
                    value: row.get(&column)?.clone(),
                    column,
                    before: text(row, "before"),
                },
            ))
        })
        .collect()
}

/// Every edit of a package's frozen column and every approval of a change, from the records
/// the state rule reads.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Edits {
    /// Each package's edits, by its key.
    pub packages: BTreeMap<String, Vec<Edit>>,
    /// Every write of a change's `approved_by`.
    pub approvals: Vec<Approval>,
    /// Each package's claimants, by its key: of every record that writes its `claim`, the
    /// claim's `by` and the record's own principal. None of them approves a change of it.
    pub claimants: BTreeMap<String, Vec<Principal>>,
    /// Of each state write, the approvals its record observed.
    heard: BTreeMap<Digest, BTreeSet<Digest>>,
}

impl Edits {
    /// Every edit and approval among `records` (each a record's operation, its changes and what
    /// it says), with what each observed of the package's state `writes` ([`super::state::writes`])
    /// and of one another. `observes` says whether the first record causally observed the second.
    pub fn of<'a>(
        records: impl IntoIterator<Item = (Digest, &'a [Change], Option<&'a Said>)>,
        writes: &BTreeMap<String, Vec<Write>>,
        observes: impl Fn(&Digest, &Digest) -> bool,
    ) -> Self {
        let mut found = Self::default();
        for (operation, changes, said) in records {
            for change in changes {
                let Ok(key) = CellKey::parse(&change.key) else {
                    continue;
                };
                if key.elem.is_some() {
                    continue;
                }
                let held = change
                    .value
                    .as_ref()
                    .and_then(|value| CellValue::decode(value).ok())
                    .map(|value| value.x);
                if change.collection == PACKAGE.name && key.col == CLAIM {
                    if let Some(claim) = held.as_ref().filter(|claim| !claim.is_null()) {
                        let of = found.claimants.entry(key.pk.to_owned()).or_default();
                        of.extend(
                            claim
                                .get("by")
                                .and_then(|by| Principal::from_value(by).ok())
                                .into_iter()
                                .chain(said.map(|said| said.principal.clone())),
                        );
                    }
                } else if change.collection == PACKAGE.name
                    && let Some(&column) = FROZEN.iter().find(|&&column| column == key.col)
                {
                    let of = found.packages.entry(key.pk.to_owned()).or_default();
                    // A record states one cell once; a second change to it replaces the first.
                    of.retain(|earlier| earlier.operation != operation || earlier.column != column);
                    of.push(Edit {
                        operation,
                        column,
                        value: held.unwrap_or(Value::Null),
                        said: said.cloned(),
                        saw: BTreeSet::new(),
                        heard_by: BTreeSet::new(),
                    });
                } else if change.collection == CHANGE.name && key.col == APPROVED_BY {
                    found.approvals.retain(|earlier| {
                        earlier.operation != operation || earlier.change != key.pk
                    });
                    found.approvals.push(Approval {
                        operation,
                        change: key.pk.to_owned(),
                        approver: held
                            .filter(|by| !by.is_null())
                            .and_then(|by| Principal::from_value(&by).ok()),
                        said: said.cloned(),
                        saw: BTreeSet::new(),
                    });
                }
            }
        }
        let approvals: Vec<Digest> = found.approvals.iter().map(|a| a.operation).collect();
        let saw = |later: &Digest, among: &mut dyn Iterator<Item = Digest>| -> BTreeSet<Digest> {
            among
                .filter(|earlier| earlier != later && observes(later, earlier))
                .collect()
        };
        let mut every: Vec<Digest> = approvals.clone();
        for (pk, of) in &mut found.packages {
            let states: Vec<Digest> = writes
                .get(pk)
                .into_iter()
                .flatten()
                .map(|write| write.operation)
                .collect();
            let edits: Vec<Digest> = of.iter().map(|edit| edit.operation).collect();
            every.extend(states.iter().chain(&edits).copied());
            for edit in of.iter_mut() {
                edit.saw = saw(
                    &edit.operation,
                    &mut states.iter().chain(&edits).chain(&approvals).copied(),
                );
                edit.heard_by = states
                    .iter()
                    .copied()
                    .filter(|state| *state != edit.operation && observes(state, &edit.operation))
                    .collect();
            }
        }
        for write in writes.values().flatten() {
            every.push(write.operation);
            found.heard.insert(
                write.operation,
                saw(&write.operation, &mut approvals.iter().copied()),
            );
        }
        every.sort_unstable();
        every.dedup();
        for approval in &mut found.approvals {
            approval.saw = saw(&approval.operation, &mut every.iter().copied());
        }
        found
    }

    /// Keep only what `keep` says of each record: the records past a writer's quota project
    /// nothing, so they are no edit and no approval either.
    pub fn retain(&mut self, keep: impl Fn(&Digest) -> bool) {
        for of in self.packages.values_mut() {
            of.retain(|edit| keep(&edit.operation));
        }
        self.packages.retain(|_, of| !of.is_empty());
        self.approvals.retain(|approval| keep(&approval.operation));
    }

    /// The approved change a write of `value` to the package `pk`'s `column` lands, if any:
    /// its record observed (`saw`) the latest approvals of a live change asking for exactly that
    /// value of that column, whose `before` is the digest of one of the values it replaces
    /// (`stood`, each with the record that wrote it; none where the package held none, and the
    /// change's `before` is then none or the digest of null), and
    /// each of those approvals approves ([`Approval::approves`]) and observed every write that
    /// stood. Its key, or none.
    pub fn lands(
        &self,
        asked: &BTreeMap<String, Asked>,
        (pk, column, value): (&str, &str, &Value),
        stood: &[(Digest, &Value)],
        saw: &BTreeSet<Digest>,
        claimants: &[Principal],
    ) -> Option<String> {
        let seen: Vec<&Approval> = self
            .approvals
            .iter()
            .filter(|approval| saw.contains(&approval.operation))
            .collect();
        let changes: BTreeSet<&str> = seen.iter().map(|a| a.change.as_str()).collect();
        changes.into_iter().find_map(|key| {
            let change = asked.get(key)?;
            let before_matches = if stood.is_empty() {
                change
                    .before
                    .as_deref()
                    .is_none_or(|before| before == digest(&Value::Null))
            } else {
                stood
                    .iter()
                    .any(|(_, was)| change.before.as_deref() == Some(digest(was).as_str()))
            };
            if change.package != pk
                || change.column != column
                || change.value != *value
                || !before_matches
            {
                return None;
            }
            let of: Vec<&&Approval> = seen.iter().filter(|a| a.change == key).collect();
            let latest: Vec<&&Approval> = of
                .iter()
                .copied()
                .filter(|a| !of.iter().any(|later| later.saw.contains(&a.operation)))
                .collect();
            latest
                .iter()
                .all(|approval| {
                    approval.approves(claimants)
                        && stood.iter().all(|(by, _)| approval.saw.contains(by))
                })
                .then(|| key.to_owned())
        })
    }

    /// Whether the state write `write` of the package `pk` withdraws the `done` that the writes
    /// `stood` made: it lands an approved change of the package's `state` asking for the state
    /// it writes, with `before` the digest of `"done"`.
    pub(super) fn withdraws(
        &self,
        asked: &BTreeMap<String, Asked>,
        pk: &str,
        write: &Write,
        stood: &[&Write],
    ) -> bool {
        let (Some(to), Some(heard)) = (write.state, self.heard.get(&write.operation)) else {
            return false;
        };
        let done = Value::from("done");
        let stood: Vec<(Digest, &Value)> = stood.iter().map(|w| (w.operation, &done)).collect();
        self.lands(
            asked,
            (pk, "state", &Value::from(to)),
            &stood,
            heard,
            self.claimants.get(pk).map_or(&[], Vec::as_slice),
        )
        .is_some()
    }
}

/// A frozen column of one package as the rule settles it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Column {
    /// The value that stands; none where edits that count, made at once, wrote different
    /// values (the table layer's conflict, which the rule does not settle).
    pub value: Option<Value>,
    /// The edits whose value stands.
    pub by: Vec<Edit>,
    /// The edits no later edit of the column saw that do not count: frozen, landing no
    /// approved change.
    pub refused: Vec<Edit>,
    /// Each edit that counts by an approved change: its record and the change's key.
    pub landed: Vec<(Digest, String)>,
}

/// Every package's frozen columns as the rule settles them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Frozen {
    /// By the package's key, then the column.
    pub packages: BTreeMap<String, BTreeMap<&'static str, Column>>,
}

impl Frozen {
    /// The column `column` of the package keyed `pk` as it settles.
    #[must_use]
    pub fn column(&self, pk: &str, column: &str) -> Option<&Column> {
        self.packages
            .get(pk)
            .and_then(|columns| columns.get(column))
    }

    /// Settle every package's edits by the state each package's records settled
    /// (`states`) and the change rows the table holds (`asked`).
    #[must_use]
    pub fn settle(
        edits: &Edits,
        asked: &BTreeMap<String, Asked>,
        states: &BTreeMap<String, Settled>,
    ) -> Self {
        let mut packages = BTreeMap::new();
        for (pk, of) in &edits.packages {
            let settled = states.get(pk);
            let freezes: &[Digest] = settled.map_or(&[], |settled| &settled.freezes);
            let stated: &[Digest] = settled.map_or(&[], |settled| &settled.stated);
            let claimants: &[Principal] = edits.claimants.get(pk).map_or(&[], Vec::as_slice);
            let mut columns = BTreeMap::new();
            for &column in FROZEN {
                let mut writes: Vec<&Edit> =
                    of.iter().filter(|edit| edit.column == column).collect();
                if writes.is_empty() {
                    continue;
                }
                // The fewer a record saw, the earlier it is: this order never judges an edit
                // before one it saw.
                writes.sort_by_key(|edit| (edit.saw.len(), edit.operation));
                let mut counted: Vec<&Edit> = Vec::new();
                let mut refused: BTreeSet<Digest> = BTreeSet::new();
                let mut landed = Vec::new();
                for &edit in &writes {
                    let stood = latest(
                        counted
                            .iter()
                            .copied()
                            .filter(|earlier| edit.saw.contains(&earlier.operation)),
                    );
                    // Frozen by a write that froze the package and did not see it, unless it is
                    // what the record stating the package's first state states.
                    let frozen = freezes.iter().any(|by| {
                        !edit.heard_by.contains(by)
                            && !(*by == edit.operation && stated.contains(by))
                    });
                    if !frozen {
                        counted.push(edit);
                        continue;
                    }
                    // Stating again the value it saw stand (null, where none did) changes
                    // nothing, and is no one's change: it only shows the edits before it were
                    // seen.
                    if stood.iter().all(|earlier| earlier.value == edit.value)
                        && (!stood.is_empty() || edit.value.is_null())
                    {
                        continue;
                    }
                    let stood: Vec<(Digest, &Value)> = stood
                        .iter()
                        .map(|earlier| (earlier.operation, &earlier.value))
                        .collect();
                    match edits.lands(
                        asked,
                        (pk.as_str(), column, &edit.value),
                        &stood,
                        &edit.saw,
                        claimants,
                    ) {
                        Some(change) => {
                            landed.push((edit.operation, change));
                            counted.push(edit);
                        }
                        None => {
                            refused.insert(edit.operation);
                        }
                    }
                }
                let by = latest(counted.iter().copied());
                let value = by
                    .first()
                    .map(|first| first.value.clone())
                    .filter(|first| by.iter().all(|edit| edit.value == *first));
                columns.insert(
                    column,
                    Column {
                        value,
                        by: by.into_iter().cloned().collect(),
                        refused: writes
                            .iter()
                            .filter(|edit| refused.contains(&edit.operation))
                            .filter(|edit| {
                                !writes
                                    .iter()
                                    .any(|later| later.saw.contains(&edit.operation))
                            })
                            .map(|edit| (*edit).clone())
                            .collect(),
                        landed,
                    },
                );
            }
            packages.insert(pk.clone(), columns);
        }
        Self { packages }
    }

    /// Give `packages`, a projection of the package table, each frozen column's settled value
    /// as the value it shows ([`cell::Row::get`]), and none where none stands (no edit counts,
    /// or edits that count made at once disagree), so a refused edit never shows. The
    /// candidates stay in the row's fields.
    pub fn over(&self, packages: &mut Projection) {
        for (pk, columns) in &self.packages {
            let Some(row) = packages.rows.get_mut(pk) else {
                continue;
            };
            for (column, settled) in columns {
                row.claims
                    .insert((*column).to_owned(), settled.value.clone());
            }
        }
    }

    /// What the rule found: a package with an edit of a frozen column that does not count
    /// (`frozen-edit`), named by the package alone.
    #[must_use]
    pub fn findings(&self) -> Vec<Finding> {
        self.packages
            .iter()
            .filter(|(_, columns)| columns.values().any(|column| !column.refused.is_empty()))
            .map(|(pk, _)| Finding {
                kind: "frozen-edit",
                collection: PACKAGE.name,
                pks: vec![pk.clone()],
            })
            .collect()
    }

    /// Whether the rule settles the package `pk`'s `column` to one value: its cell's conflict,
    /// if any, is the rule's to report, not the table layer's.
    #[must_use]
    pub fn settles(&self, pk: &str, column: &str) -> bool {
        self.column(pk, column)
            .is_some_and(|settled| settled.value.is_some())
    }
}

/// Of `writes`, the ones none of the others saw.
fn latest<'a>(writes: impl Iterator<Item = &'a Edit> + Clone) -> Vec<&'a Edit> {
    writes
        .clone()
        .filter(|edit| {
            !writes
                .clone()
                .any(|other| other.saw.contains(&edit.operation))
        })
        .collect()
}

/// What a frozen-exit finding of `kind` means, in a person's words.
pub(super) fn sentence(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "frozen-edit" => {
            "A claimed package's exit test or outcome was changed with no approved change, so \
             the change does not count and the package reads as it did before. Ask for it as a \
             change, and have someone other than its claimant approve it"
        }
        _ => return None,
    })
}

/// A frozen-exit finding of `kind`'s heading.
pub(super) fn heading(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "frozen-edit" => "Frozen exit changed",
        _ => return None,
    })
}
