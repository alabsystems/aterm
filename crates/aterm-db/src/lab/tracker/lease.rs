// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! LEASES (docs/DESIGN-lab-tracker-2026-11-14.md §4.2, decisions TK-D4 and TK-D23; package
//! TK-14): who holds a package, read alike on every machine from the records every machine
//! holds.
//!
//! - **A claim is a lease.** A package's `claim` cell names its holder, with the machine the
//!   holder's session runs on (`by.machine`, this machine's Commons uuid, stated by the claim
//!   itself: the cell is rewritten by whoever settles a contest, so the record that holds the
//!   cell now need not be the claiming machine's).
//! - **A machine renews its leases.** It appends one signed entry to the tracker's `renewal`
//!   log ([`RENEWAL`]) every [`RENEW_EVERY_MS`] while any of its sessions holds a lease
//!   ([`renew`]), each stating the period it keeps them for ([`PERIOD_MS`]).
//! - **A lease lapses** when its machine's newest renewal is older than the period that renewal
//!   states, and the claim itself older than [`PERIOD_MS`] (a fresh claim stands before its
//!   machine's next renewal). Lapse is read from replicated records only, so every machine that
//!   holds the same records reads the same holders at the same time; no machine's record of whom
//!   it heard from is read. A lapsed claim stays in its cell, shown as lapsed, until another
//!   agent takes the package over: then the package is free to take ([`Leases::next`]).
//! - **Concurrent claims are contested.** Two claims written at once both stand in the cell, and
//!   both are shown ([`Lease::contested`]). The one that keeps it ([`Lease::holder`]) is the one
//!   whose holder has already written to the package (a cell of its row other than its state
//!   and claim, or a note about it); among those, or where neither has, the one whose record has
//!   the smaller operation digest: an order every machine reads alike and that favours no
//!   machine's clock. The other agent releases ([`Leases::held_by`]): `aterm goals claim --next`
//!   does it for it, writing the keeper's claim back as the cell's one value.
//! - **`claim --next` spreads agents** ([`Leases::next`]): of the ready packages nobody holds,
//!   in rank order, it takes one of the first [`NEXT_AMONG`] by a hash of the agent, so a hundred
//!   agents asking at once land on different packages instead of all on the first.
//!
//! Claims across a LAN split cannot be exclusive, and this does not pretend they are: each side
//! hands out what it reads as free, and the two claims are contested once the sides meet.
//! Nothing here is stored: every reader computes the same leases from the same records
//! ([`Leases::of`]).

use super::state::States;
use super::tables::PACKAGE;
use crate::typed::{
    Collection, Column, Derive, Kind, MAX_OWNED_ROW, Ty, Writer, Writers,
    cell::{Field, Projection},
    keys,
    owned::LogRead,
    values::Principal,
};
use astream_wire::sync::Digest;
use aterm_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// How long a renewal keeps its machine's leases, as each renewal states it, in milliseconds;
/// a claim with no renewal after it stands this long from when it was made.
pub const PERIOD_MS: u64 = 30 * 60 * 1000;
/// How often a machine that holds a lease renews it: a third of the period, so a renewal two
/// late still keeps the lease.
pub const RENEW_EVERY_MS: u64 = PERIOD_MS / 3;
/// How many of the first ready, unheld packages `claim --next` chooses among.
pub const NEXT_AMONG: usize = 8;

/// The package columns a lease reads.
const CLAIM: &str = "claim";
const STATE: &str = "state";

crate::att_collection!(
    /// One renewal of a machine's leases (§4.2): `<uuid32>.<n>`, when the machine renewed and
    /// how long the renewal keeps its leases, in milliseconds. A machine appends one a period
    /// while any of its sessions holds a lease ([`renew`]); its newest is what keeps them.
    pub RENEWAL = Collection {
        name: "renewal",
        version: 1,
        kind: Kind::Log,
        writers: Writers::SelfSuffix,
        prefix: "",
        derive: Derive::Composite,
        columns: &[
            Column::new("n", Ty::Uint).writer(Writer::Mine),
            Column::new("at", Ty::Time).writer(Writer::Mine),
            Column::new("period", Ty::Uint).writer(Writer::Mine),
        ],
        unique: &[],
        max_row: MAX_OWNED_ROW,
        // An entry is its signer's own statement at its own key: no cell is judged by the
        // authority rules.
        authority: None,
    }
);

/// One claim of a package, as its cell holds it.
#[derive(Clone, Debug, PartialEq)]
pub struct Claim {
    /// The record whose candidate of the cell this is.
    pub operation: Digest,
    /// Who holds it.
    pub by: Principal,
    /// The machine whose renewals keep it: the claim's own `by.machine`, a 32-digit uuid.
    pub machine: Option<String>,
    /// The cell's value as written: what a release that settles a contest writes back.
    pub value: Value,
    /// When it stops standing without a newer renewal, Unix milliseconds.
    pub until: u64,
    /// Whether its holder has written to the package besides claiming it: a cell of its row
    /// other than its state and claim, or a note about it.
    pub worked: bool,
}

/// Who holds one package.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Lease {
    /// The claims that stand, the one that keeps the package first.
    pub held: Vec<Claim>,
    /// The claims that lapsed.
    pub lapsed: Vec<Claim>,
}

impl Lease {
    /// The claim that keeps the package: the one standing claim, or of contested ones the one
    /// whose holder has written to the package, else the smaller operation.
    #[must_use]
    pub fn holder(&self) -> Option<&Claim> {
        self.held.first()
    }

    /// Whether more than one claim stands: the board's `contested`.
    #[must_use]
    pub fn contested(&self) -> bool {
        self.held.len() > 1
    }
}

/// Whether two principals are one agent: the same aterm session where both name one (and the
/// same program where both name one), else the same statement on the same machine.
#[must_use]
pub fn same_agent(a: &Principal, b: &Principal) -> bool {
    match (&a.session, &b.session) {
        (Some(x), Some(y)) => {
            x == y
                && match (&a.program, &b.program) {
                    (Some(p), Some(q)) => p == q,
                    _ => true,
                }
        }
        // Outside a session: the same statement, on the same machine where both name one (a
        // record's own principal names none; a claim's names its machine).
        _ => {
            let bare = |p: &Principal| Principal {
                machine: None,
                ..p.clone()
            };
            bare(a) == bare(b)
                && match (&a.machine, &b.machine) {
                    (Some(x), Some(y)) => x == y,
                    _ => true,
                }
        }
    }
}

/// What a write to a package says of who did it: the package's key, and the principal of the
/// record that wrote a cell of its row other than its state and claim, or a note about it.
pub type Work = Vec<(String, Principal)>;

/// Every package's lease, and each machine's newest renewal.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Leases {
    /// Each package that holds a claim, by its key.
    pub packages: BTreeMap<String, Lease>,
    /// Each machine's newest renewal, by its 32-digit uuid: when, and the period it states.
    pub renewed: BTreeMap<String, (u64, u64)>,
    /// The reader's clock the leases were read at, Unix milliseconds.
    pub now_ms: u64,
}

impl Leases {
    /// The leases `packages` (a projection of the package table) hold at `now_ms`, kept by the
    /// `renewals` log's entries, with `work` the writes to each package that say who made them.
    /// Only a running package is leased, by its settled state (`states`, else as its row states
    /// it): a claim left in the cell of a done, blocked, dropped or ready package holds nothing.
    #[must_use]
    pub fn of(
        packages: &Projection,
        states: &States,
        renewals: Option<&LogRead>,
        work: &Work,
        now_ms: u64,
    ) -> Self {
        let mut renewed: BTreeMap<String, (u64, u64)> = BTreeMap::new();
        for entry in renewals
            .map(|log| log.entries.as_slice())
            .unwrap_or_default()
        {
            let (Some(at), Some(period)) = (
                entry.value.get("at").and_then(Value::as_u64),
                entry.value.get("period").and_then(Value::as_u64),
            ) else {
                continue;
            };
            let newest = renewed
                .entry(keys::uuid32(&entry.machine))
                .or_insert((at, period));
            if at > newest.0 {
                *newest = (at, period);
            }
        }
        let mut leases = BTreeMap::new();
        for (pk, row) in &packages.rows {
            let state = states
                .state(pk)
                .or_else(|| row.get(STATE).and_then(Value::as_str));
            if !row.live() || state != Some("running") {
                continue;
            }
            let Some(field) = row.fields.get(CLAIM) else {
                continue;
            };
            let candidates = match field {
                Field::Value(_) | Field::Conflict { .. } => field.candidates(),
                Field::Set(_) => continue,
            };
            let mut lease = Lease::default();
            for candidate in candidates {
                let value = &candidate.value.x;
                let Some(by) = value
                    .get("by")
                    .and_then(|by| Principal::from_value(by).ok())
                else {
                    continue;
                };
                let machine = by.machine.clone();
                // When the claim was made: its millisecond, else the start of its day.
                let made = value.get("at").and_then(Value::as_u64).or_else(|| {
                    value
                        .get("on")
                        .and_then(Value::as_u64)
                        .map(|day| day.saturating_mul(86_400_000))
                });
                let renewal = machine
                    .as_ref()
                    .and_then(|machine| renewed.get(machine))
                    .map_or(0, |(at, period)| at.saturating_add(*period));
                let until = made
                    .map_or(0, |made| made.saturating_add(PERIOD_MS))
                    .max(renewal);
                let worked = work
                    .iter()
                    .any(|(of, who)| of == pk && same_agent(who, &by));
                let claim = Claim {
                    operation: candidate.operation,
                    by,
                    machine,
                    value: value.clone(),
                    until,
                    worked,
                };
                if now_ms < until {
                    lease.held.push(claim);
                } else {
                    lease.lapsed.push(claim);
                }
            }
            if lease.held.is_empty() && lease.lapsed.is_empty() {
                continue;
            }
            // The one that keeps it first: a holder who has written to the package, then the
            // smaller operation.
            lease
                .held
                .sort_by_key(|claim| (!claim.worked, claim.operation));
            leases.insert(pk.clone(), lease);
        }
        Self {
            packages: leases,
            renewed,
            now_ms,
        }
    }

    /// The lease of the package keyed `pk`.
    #[must_use]
    pub fn of_package(&self, pk: &str) -> Option<&Lease> {
        self.packages.get(pk)
    }

    /// Whether anyone holds the package keyed `pk`: a claim of it stands.
    #[must_use]
    pub fn held(&self, pk: &str) -> bool {
        self.packages
            .get(pk)
            .is_some_and(|lease| !lease.held.is_empty())
    }

    /// Each package `agent` holds a standing claim of, by its key, with whether that claim is
    /// the one that keeps it: a contested claim that does not keep it is the one its agent
    /// releases.
    #[must_use]
    pub fn held_by(&self, agent: &Principal) -> Vec<(&str, bool)> {
        self.packages
            .iter()
            .filter_map(|(pk, lease)| {
                let at = lease
                    .held
                    .iter()
                    .position(|claim| same_agent(&claim.by, agent))?;
                Some((pk.as_str(), at == 0))
            })
            .collect()
    }

    /// The packages more than one standing claim holds: the board's `contested`.
    #[must_use]
    pub fn contested(&self) -> Vec<&str> {
        self.packages
            .iter()
            .filter(|(_, lease)| lease.contested())
            .map(|(pk, _)| pk.as_str())
            .collect()
    }

    /// Whether the package keyed `pk`, whose settled state is `state`, may be claimed: ready and
    /// held by no one, or running with every claim of it lapsed (a takeover).
    #[must_use]
    pub fn free(&self, pk: &str, state: Option<&str>) -> bool {
        match state {
            Some("ready") => !self.held(pk),
            Some("running") => self
                .packages
                .get(pk)
                .is_none_or(|lease| lease.held.is_empty()),
            _ => false,
        }
    }

    /// The package `claim --next` gives `agent` (§4.2): of the live packages `packages` holds
    /// in `lane` (any lane where none is named) that [`Self::free`] takes by their settled
    /// `states`, in rank order (a package with no rank after every ranked one, then by key),
    /// the one a hash of the agent picks among the first [`NEXT_AMONG`]. `None` where none is
    /// free.
    #[must_use]
    pub fn next(
        &self,
        states: &States,
        packages: &Projection,
        agent: &Principal,
        lane: Option<&str>,
    ) -> Option<String> {
        let rank = |row: &crate::typed::cell::Row| {
            row.get("rank")
                .and_then(|rank| match rank {
                    Value::String(text) => text.parse::<f64>().ok(),
                    other => other.as_f64(),
                })
                .filter(|rank| rank.is_finite())
        };
        let mut free: Vec<(Option<f64>, &str)> = packages
            .rows
            .iter()
            .filter(|(_, row)| row.live())
            .filter(|(_, row)| {
                lane.is_none_or(|lane| row.get("lane").and_then(Value::as_str) == Some(lane))
            })
            .filter(|(pk, row)| {
                let state = states
                    .state(pk)
                    .or_else(|| row.get(STATE).and_then(Value::as_str));
                self.free(pk, state)
            })
            .map(|(pk, row)| (rank(row), pk.as_str()))
            .collect();
        free.sort_by(|(a, x), (b, y)| match (a, b) {
            (Some(a), Some(b)) => a.total_cmp(b).then_with(|| x.cmp(y)),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => x.cmp(y),
        });
        free.truncate(NEXT_AMONG);
        if free.is_empty() {
            return None;
        }
        let at = spread(agent) % free.len() as u64;
        free.get(usize::try_from(at).ok()?)
            .map(|(_, pk)| (*pk).to_owned())
    }
}

/// A hash of `agent` that spreads agents over the packages `claim --next` chooses among: of
/// its session where it names one, else of its whole statement, its machine included.
fn spread(agent: &Principal) -> u64 {
    let name = match &agent.session {
        Some(session) => session.clone(),
        None => agent.to_value().to_string(),
    };
    let digest = aterm_digest::Sha256::digest(name.as_bytes());
    let mut head = [0; 8];
    head.copy_from_slice(&digest[..8]);
    u64::from_be_bytes(head)
}

/// The writes to packages among `records` (each a record's operation, its changes and what it
/// says) that say who made them, other than of a package's state and claim, with the notes
/// `notes` holds about a package: the work [`Leases::of`] reads. A record that says nothing is
/// no one's work.
pub fn work<'a>(
    records: impl IntoIterator<
        Item = (
            Digest,
            &'a [crate::database::Change],
            Option<&'a crate::said::Said>,
        ),
    >,
    notes: Option<&LogRead>,
) -> Work {
    let mut by: BTreeMap<Digest, Principal> = BTreeMap::new();
    let mut found: BTreeSet<(String, Digest)> = BTreeSet::new();
    for (operation, changes, said) in records {
        let Some(said) = said else { continue };
        by.insert(operation, said.principal.clone());
        for change in changes {
            if change.collection != PACKAGE.name {
                continue;
            }
            let Ok(key) = crate::typed::cell::CellKey::parse(&change.key) else {
                continue;
            };
            if key.col != STATE && key.col != CLAIM {
                found.insert((key.pk.to_owned(), operation));
            }
        }
    }
    for entry in notes.map(|log| log.entries.as_slice()).unwrap_or_default() {
        if entry.value.get("on").and_then(Value::as_str) != Some(PACKAGE.name) {
            continue;
        }
        if let Some(about) = entry.value.get("about").and_then(Value::as_str) {
            found.insert((about.to_owned(), entry.operation));
        }
    }
    found
        .into_iter()
        .filter_map(|(pk, operation)| Some((pk, by.get(&operation)?.clone())))
        .collect()
}

/// One read of the tracker with every package's state settled and its lease read at `now_ms`:
/// the table's view, the states ([`super::state::read`]) and the leases, from the same log.
///
/// # Errors
/// As [`crate::att::Handle::view`].
#[cfg(unix)]
pub fn read(
    handle: &crate::att::Handle,
    now_ms: u64,
) -> crate::Result<(crate::att::View, States, Leases)> {
    use super::state;
    let (view, _, (mut found, mut edits, mut records)) = handle.usage_with(|_, log| {
        let records: Vec<(
            Digest,
            Vec<crate::database::Change>,
            Option<crate::said::Said>,
        )> = log
            .eligible_iter()
            .filter_map(|(operation, command)| {
                Some((
                    operation.header.id,
                    command.changes()?.to_vec(),
                    command.said().cloned(),
                ))
            })
            .collect();
        let borrowed = || {
            records
                .iter()
                .map(|(operation, changes, said)| (*operation, changes.as_slice(), said.as_ref()))
        };
        let observes = |later: &Digest, earlier: &Digest| log.observes(later, earlier);
        let found = state::writes(borrowed(), observes);
        let edits = super::frozen::Edits::of(borrowed(), &found, observes);
        (found, edits, records)
    })?;
    // The records past a writer's quota project nothing, so they write no state and no work.
    if !view.quota.over.is_empty() {
        for of in found.values_mut() {
            of.retain(|write| !view.quota.over.contains(&write.operation));
        }
        found.retain(|_, of| !of.is_empty());
        edits.retain(|operation| !view.quota.over.contains(operation));
        records.retain(|(operation, _, _)| !view.quota.over.contains(operation));
    }
    let states = state::settle(found, &edits, &view.cells);
    let work = work(
        records
            .iter()
            .map(|(operation, changes, said)| (*operation, changes.as_slice(), said.as_ref())),
        view.logs.get(super::NOTE.name),
    );
    let leases = Leases::of(
        &view.project(&PACKAGE),
        &states,
        view.logs.get(RENEWAL.name),
        &work,
        now_ms,
    );
    Ok((view, states, leases))
}

/// What a renewal pass did ([`renew`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Renewed {
    /// No session of this machine that `live` names holds a lease: nothing to keep.
    Nothing,
    /// This machine's newest renewal is younger than [`RENEW_EVERY_MS`]: nothing written.
    Fresh,
    /// One renewal appended, at this key.
    Wrote(String),
}

/// Renew this machine's leases (§4.2, TK-D23): where any claim that names this machine
/// (`machine`, its 32-digit uuid) is held by an agent `live` says still runs here, and this
/// machine's newest renewal is [`RENEW_EVERY_MS`] old or older, append one renewal at `now_ms`
/// stating [`PERIOD_MS`]. A lapsed claim of a live session is renewed too: it stands again
/// unless another agent took the package over meanwhile. The instance that hosts the sessions
/// calls it from their lifecycle.
///
/// # Errors
/// The tracker cannot be read, or the renewal cannot be appended
/// ([`crate::att::Handle::append_log`]).
#[cfg(unix)]
pub fn renew(
    handle: &crate::att::Handle,
    machine: &str,
    now_ms: u64,
    live: impl Fn(&Principal) -> bool,
) -> crate::Result<Renewed> {
    let (_, _, leases) = read(handle, now_ms)?;
    let holds = leases.packages.values().any(|lease| {
        lease
            .held
            .iter()
            .chain(&lease.lapsed)
            .any(|claim| claim.machine.as_deref() == Some(machine) && live(&claim.by))
    });
    if !holds {
        return Ok(Renewed::Nothing);
    }
    if leases
        .renewed
        .get(machine)
        .is_some_and(|(at, _)| now_ms.saturating_sub(*at) < RENEW_EVERY_MS)
    {
        return Ok(Renewed::Fresh);
    }
    let mut entry = aterm_json::Map::new();
    entry.insert("at".to_owned(), Value::from(now_ms));
    entry.insert("period".to_owned(), Value::from(PERIOD_MS));
    let key = handle
        .append_log(&RENEWAL, &[entry])?
        .pop()
        .ok_or_else(|| crate::Error::new("the append returned no key"))?;
    Ok(Renewed::Wrote(key))
}
