// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Following a link to a registry machine (docs/DESIGN-lab-tracker-2026-11-14.md §4.6, package
//! TK-07): what a row of another table family means when it names a machine.
//!
//! A machine is named by its 32-digit Commons machine uuid ([`MACHINE_LINK`]), the key of the
//! `machine-facts` row that machine states of itself, never by a label, which is neither unique
//! nor stable. The uuid is read, each time, through the registry as it stands then
//! ([`Machines::of`], at whichever generation holds the registry's rows):
//!
//! 1. **Facts.** The writer's own facts row binds a machine row ([`bound_machine`]).
//! 2. **Writer lineage.** A writer succession gives one machine a new uuid, and the successor's
//!    facts row names the one before it (`predecessor`). A writer that states no facts row, or
//!    whose row binds nothing, is the machine its successor is ([`bound`]).
//! 3. **Merges.** A merge a machine has acknowledged is in its own facts row, which names the
//!    row it was merged into ([`bound_machine`] reads that first). An earlier writer of the
//!    lineage whose facts still bind the merged row follows that row's merge where a writer of
//!    its lineage states the same merge, however many such merges on. A machine row's
//!    `merged_into` alone moves no machine: it is a cell any writer can set, so it is
//!    display-only until the machine's own lineage says the same
//!    (docs/DESIGN-alab-lab-model-2026-11-01.md §6), as the registrar holds it
//!    (`register::followed`).
//!
//! What comes of it is a [`Machine`]: a live row, or what a check reports (a retired row, a
//! merged row no lineage states or one whose state cannot be read, a row the registry does not
//! hold, a writer nothing binds, which reads as its uuid).
//!
//! **The same machine, or another** ([`Machines::same`]) is equality of the machine row two
//! writers come to. Where either comes to none (a Linux member before LT-C6, a debug build, a
//! machine alone on its LAN), or to a row whose merge is unsettled, the question has no answer,
//! and each reader takes the side that
//! claims nothing: a lease compares the two uuids through writer lineage
//! ([`Machines::one_writer`]), an evidence grade stays `self-run` ([`Same::another`] is false),
//! and a `same-machine` comparison is a finding (`lab::tracker::checks::links`).
//!
//! **A host name** in an imported record ([`Machines::host`]) is found among the hosts each
//! machine states of itself in its facts row, which only that machine can write, never among
//! the machine rows' labels.

use super::register::same_host;
use super::registry::{MACHINE_FACTS, MACHINE_LINK, bound_machine};
use crate::typed::{
    LIFECYCLE, View,
    cell::{self, Projection, Row},
    keys,
    owned::OwnedRead,
};
use aterm_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// The machine row each Commons machine's facts bind ([`bound_machine`]: its `merged_into`,
/// else its `mac`), by the Commons machine that states them. A facts row's `predecessor` is the
/// same machine before a writer succession: a predecessor whose own facts row binds no machine
/// row, or that has none, binds what its successor binds, however far back the chain runs.
pub fn bound(facts: &OwnedRead) -> BTreeMap<[u8; 16], String> {
    let mut bound: BTreeMap<[u8; 16], String> = facts
        .rows
        .values()
        .filter_map(|row| {
            let key = bound_machine(&row.value).filter(|key| !key.is_empty())?;
            Some((row.machine, key.to_owned()))
        })
        .collect();
    // Each pass carries a binding one predecessor back; a chain is no longer than the rows.
    for _ in 0..facts.rows.len() {
        let mut carried = false;
        for row in facts.rows.values() {
            let Some(before) = predecessor(&row.value) else {
                continue;
            };
            if bound.contains_key(&before) {
                continue;
            }
            if let Some(key) = bound.get(&row.machine).cloned() {
                bound.insert(before, key);
                carried = true;
            }
        }
        if !carried {
            break;
        }
    }
    bound
}

/// The writer a facts row says its machine wrote as before.
fn predecessor(facts: &Value) -> Option<[u8; 16]> {
    facts
        .get("predecessor")
        .and_then(Value::as_str)
        .and_then(keys::parse_uuid32)
}

/// What a machine row is to a link that reaches it.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Life {
    Live,
    /// Merged into the row of this key: its lifecycle says `merged` and its `merged_into` names
    /// a row, neither in conflict.
    Merged(String),
    /// Retired, ended or locked: its lifecycle says so, in no conflict.
    Ended,
    /// Not live, and what it is cannot be read: a lifecycle in conflict or in a newer build's
    /// word, or a merge that names no row this build reads.
    Unread,
}

impl Life {
    fn of(row: &Row) -> Self {
        if row.live() {
            return Self::Live;
        }
        let settled = |column: &str| {
            !row.raw.contains(column)
                && row
                    .fields
                    .get(column)
                    .is_some_and(|field| !field.conflicted())
        };
        match row.lifecycle().filter(|_| settled(LIFECYCLE)) {
            Some("merged") => row
                .get("merged_into")
                .and_then(Value::as_str)
                .filter(|_| settled("merged_into"))
                .map_or(Self::Unread, |into| Self::Merged(into.to_owned())),
            Some(_) => Self::Ended,
            None => Self::Unread,
        }
    }
}

/// What a machine's uuid comes to in the registry ([`Machines::resolve`]).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Machine {
    /// The live machine row, by its key.
    Row(String),
    /// A machine row that is retired, ended or locked: the row, by its key.
    Retired(String),
    /// A machine row that is not live and does not say which machine this is, by its key: it
    /// was merged and no writer of this machine's lineage states that merge, its merges come
    /// back to it, or its lifecycle or merge cannot be read (a conflict, a newer build's word).
    Unsettled(String),
    /// A machine row the registry does not hold, by the key a facts row or a merge names.
    Missing(String),
    /// No facts row binds the writer, its own or a successor's: the machine reads as its uuid.
    Unbound,
}

impl Machine {
    /// The machine row this is, live or retired: what two writers are compared by. A row whose
    /// merge is unsettled is not known to be this machine or another, so it is none.
    #[must_use]
    pub fn row(&self) -> Option<&str> {
        match self {
            Self::Row(key) | Self::Retired(key) => Some(key),
            Self::Unsettled(_) | Self::Missing(_) | Self::Unbound => None,
        }
    }
}

/// Whether two writers are one machine ([`Machines::same`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Same {
    /// Both come to one machine row.
    Same,
    /// Each comes to a machine row, and the two differ.
    Another,
    /// One of them comes to no machine row, so nothing says.
    Unanswered,
}

impl Same {
    /// Whether the two are known to be one machine. Unanswered is not.
    #[must_use]
    pub const fn same(self) -> bool {
        matches!(self, Self::Same)
    }

    /// Whether the two are known to be different machines, as an `independent` evidence grade
    /// needs of the machine that ran a test and the one that claimed it (design §4.4).
    /// Unanswered is not, so that grade stays `self-run`.
    #[must_use]
    pub const fn another(self) -> bool {
        matches!(self, Self::Another)
    }
}

/// The machine a host name of an imported record names ([`Machines::host`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Host {
    /// One machine states the name: its uuid, the newest writer of its lineage that states it.
    One(String),
    /// No machine states it. The import keeps the text and reports it.
    None,
    /// Machines of different lineages state it, by their uuids. The import keeps the text and
    /// reports it.
    Many(Vec<String>),
}

/// The registry's machines as a link reads them: one read of its facts rows and machine rows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Machines {
    /// The machine row each writer's facts bind, a predecessor's through its successor
    /// ([`bound`]).
    bound: BTreeMap<[u8; 16], String>,
    /// The writer each facts row names as the one before it.
    before: BTreeMap<[u8; 16], [u8; 16]>,
    /// Each machine row the registry holds.
    rows: BTreeMap<String, Life>,
    /// Each merge a facts row states of its own machine: the writer, the row it binds (`mac`)
    /// and the row it says that one was merged into (`merged_into`).
    merges: Vec<([u8; 16], String, String)>,
    /// The host names each writer states of its own machine.
    hosts: BTreeMap<[u8; 16], Vec<String>>,
}

impl Machines {
    /// The machines `view` holds, where it is a read of the registry: of any generation of the
    /// family [`MACHINE_LINK`] names, the `machine` collection taken as that generation
    /// declares it. `None` for a read of any other table. A caller follows the link through
    /// the registry's generations by reading the one that holds its rows now
    /// (`att::current`, as the registry's own read does).
    #[must_use]
    pub fn of(view: &View) -> Option<Self> {
        let table = view.table.filter(|table| MACHINE_LINK.follows(table))?;
        let machines = table.collection(MACHINE_LINK.collection)?;
        let none = OwnedRead::default();
        Some(Self::from_rows(
            // Only a row's lifecycle and merge are read, and neither is a claimed column, so
            // the cells need no claim settled.
            &cell::project(machines, &view.cells),
            view.owned.get(MACHINE_FACTS.name).unwrap_or(&none),
        ))
    }

    /// The machines `facts` (each machine's own statement, keyed by its Commons machine uuid)
    /// and `machines` (the registry's machine rows) hold.
    #[must_use]
    pub fn from_rows(machines: &Projection, facts: &OwnedRead) -> Self {
        Self {
            bound: bound(facts),
            before: facts
                .rows
                .values()
                .filter_map(|row| Some((row.machine, predecessor(&row.value)?)))
                .collect(),
            rows: machines
                .rows
                .iter()
                .map(|(key, row)| (key.clone(), Life::of(row)))
                .collect(),
            merges: facts
                .rows
                .values()
                .filter_map(|row| {
                    let text = |column: &str| row.value.get(column).and_then(Value::as_str);
                    Some((
                        row.machine,
                        text("mac")?.to_owned(),
                        text("merged_into")?.to_owned(),
                    ))
                })
                .collect(),
            hosts: facts
                .rows
                .values()
                .map(|row| {
                    let hosts = row
                        .value
                        .get("hosts")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect();
                    (row.machine, hosts)
                })
                .collect(),
        }
    }

    /// What the machine `uuid` names comes to: the row its facts bind (its own, or its
    /// successor's), then each row that one was merged into by a merge a writer of the same
    /// lineage states. Text that is no machine uuid names no writer, so nothing binds it.
    #[must_use]
    pub fn resolve(&self, uuid: &str) -> Machine {
        let Some(writer) = keys::parse_uuid32(uuid) else {
            return Machine::Unbound;
        };
        let Some(mut at) = self.bound.get(&writer) else {
            return Machine::Unbound;
        };
        let mut passed = BTreeSet::new();
        loop {
            match self.rows.get(at) {
                None => return Machine::Missing(at.clone()),
                Some(Life::Live) => return Machine::Row(at.clone()),
                Some(Life::Ended) => return Machine::Retired(at.clone()),
                // The row's own cells move no machine: a writer of this lineage states the
                // merge, or the link stays on the merged row. A circle of merges stops where
                // it began.
                Some(Life::Merged(into))
                    if passed.insert(at) && self.states_merge(writer, at, into) =>
                {
                    at = into;
                }
                Some(Life::Merged(_) | Life::Unread) => return Machine::Unsettled(at.clone()),
            }
        }
    }

    /// Whether a facts row of `writer`'s lineage states that the row `mac` was merged into
    /// `into`: the machine's own signed word for the merge the row's cells show.
    fn states_merge(&self, writer: [u8; 16], mac: &str, into: &str) -> bool {
        self.merges.iter().any(|(stater, stated_mac, stated_into)| {
            stated_mac == mac && stated_into == into && self.one_lineage(*stater, writer)
        })
    }

    /// Whether `a` and `b` are one writer or two of one lineage, one of them a writer before
    /// the other. Two successors of one writer (a copied home) are not: each is its own machine.
    fn one_lineage(&self, a: [u8; 16], b: [u8; 16]) -> bool {
        a == b || self.lineage(a).contains(&b) || self.lineage(b).contains(&a)
    }

    /// Whether the machines `a` and `b` name are one: equality of the machine rows they come to
    /// ([`Machine::row`]), and unanswered where either comes to none or to a row whose merge is
    /// unsettled, the same uuid twice included.
    #[must_use]
    pub fn same(&self, a: &str, b: &str) -> Same {
        match (self.resolve(a).row(), self.resolve(b).row()) {
            (Some(a), Some(b)) if a == b => Same::Same,
            (Some(_), Some(_)) => Same::Another,
            _ => Same::Unanswered,
        }
    }

    /// `writer`, then each writer before it, as the facts rows name them: one machine's
    /// writers, newest first. The walk stops at a writer it passed.
    fn lineage(&self, writer: [u8; 16]) -> Vec<[u8; 16]> {
        let mut lineage = vec![writer];
        let mut at = writer;
        while let Some(before) = self.before.get(&at).filter(|b| !lineage.contains(b)) {
            lineage.push(*before);
            at = *before;
        }
        lineage
    }

    /// Whether the uuids `a` and `b` are one writer or two of one lineage
    /// ([`Self::one_lineage`]): what a lease compares its holder by, which needs no facts row
    /// of either (the same uuid is the same writer) and claims nothing of two writers no facts
    /// row joins.
    #[must_use]
    pub fn one_writer(&self, a: &str, b: &str) -> bool {
        let (Some(a), Some(b)) = (keys::parse_uuid32(a), keys::parse_uuid32(b)) else {
            return false;
        };
        self.one_lineage(a, b)
    }

    /// The machine that states the host `name` in its own facts row, with or without `.local`
    /// and in any case (`register::same_host`). Writers of one lineage that state it are one
    /// machine, named by the newest of them.
    #[must_use]
    pub fn host(&self, name: &str) -> Host {
        let stating: Vec<[u8; 16]> = self
            .hosts
            .iter()
            .filter(|(_, hosts)| hosts.iter().any(|host| same_host(host, name)))
            .map(|(writer, _)| *writer)
            .collect();
        let newest: Vec<String> = stating
            .iter()
            .filter(|writer| {
                !stating
                    .iter()
                    .any(|other| other != *writer && self.lineage(*other).contains(writer))
            })
            .map(keys::uuid32)
            .collect();
        match newest.as_slice() {
            [] => Host::None,
            [one] => Host::One(one.clone()),
            _ => Host::Many(newest),
        }
    }
}
