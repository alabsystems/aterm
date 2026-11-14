// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! ACCOUNT CLAIMS ON THE LAB'S DAY TABLES (docs/DESIGN-fluid-swarm-2026-10-28.md F19): one row per
//! Mac, the whole set of accounts it claims, each signed by the Mac's own Commons writer. A row's
//! key is its signer's machine, so no Mac can write another Mac's row: a row at any other key is
//! inert. The row's body is the harness's (`aterm_agent::harness::claims::Row`); a table holds it
//! whole, at most [`MAX_ROW`] bytes, and gives each writer's latest one back. A fresh row advances
//! its counter exactly once under the table writer, so a stale whole-set replacement cannot erase
//! another local client's claims. Any current Writer writes its row; nothing brokers. A row an
//! earlier writer generation of this home signed (its seat stays a member after a writer
//! succession) is this Mac's past: this Mac's read gives it apart from the members' rows
//! ([`Read::former`]), so its claims' seniority can go on in the current seat's row, and every
//! other Mac reads it as any peer's row until it lapses.
//!
//! Rows ride a ring of UTC-day tables (`crate::catalog::ring`, [`EPOCH_S`]) that no catalog
//! declares, so nothing fills however long the lab runs. A row goes into its writer's day table,
//! or a later table of its own, never earlier than the day before the one its `until` falls in, so
//! every row of a table lapses before the day after next begins; each Mac deletes a day's table
//! then, on its own clock. Of two rows from one Mac the one in the later table wins, then the
//! higher counter. A day table takes [`Caps`] records, then the next day's.
//! Revision and retry checks cover every table read, under one writer.
//!
//! An F19 build from before the ring wrote `accounts/claims/1`, rolling to `claims.2` through
//! `claims.6`. A ring Mac that finds those declarations declares each table at version 2, once
//! (`Directory::seal`): the old build's reads and writes then fail, so it keeps to its home
//! accounts, and no generation stays subscribed to take a catalog slot. Ring Macs read the old
//! tables, ranked below every ring row, while the old table is still subscribed and for two days
//! after the seal.

use crate::{
    Error, Result,
    catalog::{
        Descriptor, Directory, Snapshot as CatalogSnapshot, State,
        ring::{self, CLAIMS, RingState, Shares, Unplaced, Used},
    },
    database::{Change, Command},
    documents::log::Log,
    table::{Session, store::Store},
};
use astream_wire::sync::Actor;
use aterm_codec::hex;
use aterm_digest::Sha256;
use aterm_json::Value;
use aterm_secrets::{client::Paths, commons::bootstrap::Bootstrap};
use std::{collections::BTreeMap, time::Instant};

/// A row's canonical body is at most this long.
pub const MAX_ROW: usize = crate::crypto::MAX_BODY - 4096;
/// The length of one claims table's period: one table per UTC day. A row written now must lapse
/// within this long (the harness's `HORIZON_S`) to land in the current day's table.
pub const EPOCH_S: u64 = 86_400;
/// The tables an F19 build from before the ring rolled through (`claims` .. `claims.6`), read only
/// while that build's table counts.
const LEGACY_GENERATIONS: u32 = 6;
const COLLECTION: &str = "row";
const COMMAND_DOMAIN: &[u8] = b"aterm/accounts/claims/command/1\0";

/// How many retained records one day table takes, in all and from one writer,
/// before a write spills into the next day's: below the store's 8192 records,
/// so the two sides of a split each fill no more than
/// a heal can merge in most of it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Caps {
    pub records: usize,
    pub writer_records: usize,
}

impl Default for Caps {
    fn default() -> Self {
        Self {
            records: 7_680,
            writer_records: 960,
        }
    }
}

impl Caps {
    /// Leave headroom below the store's table and per-writer byte limits.
    const fn shares(self) -> Shares {
        Shares {
            records: self.records,
            bytes: 115 * 1024 * 1024,
            writer_records: self.writer_records,
            writer_bytes: 6 * 1024 * 1024,
        }
    }
}

/// The table a row was read from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Source {
    /// An F19 build's rolled table from before the ring, by generation.
    Legacy(u32),
    /// The ring's table of a day.
    Ring(u64),
}

impl Source {
    /// Tables rank first: every ring table above every legacy one, a later day above an earlier.
    const fn tier(self) -> (u64, u32) {
        match self {
            Self::Legacy(generation) => (0, generation),
            Self::Ring(epoch) => (epoch.saturating_add(1), 0),
        }
    }
}

fn legacy_descriptor(bootstrap: &Bootstrap, generation: u32) -> Result<Descriptor> {
    legacy_descriptor_in(bootstrap.trust(), generation)
}

fn legacy_descriptor_in(
    trust: aterm_secrets::commons::journal::Trust,
    generation: u32,
) -> Result<Descriptor> {
    if generation <= 1 {
        Descriptor::application(trust, "accounts", "claims", 1)
    } else {
        Descriptor::application(trust, "accounts", &format!("claims.{generation}"), 1)
    }
}

/// Every table an F19 build from before the ring declared (`claims`, `claims.2` .. `claims.6`):
/// what each ring Mac's claims write seals, and what the storage release frees once their rows
/// have lapsed (`catalog::release`).
pub(crate) fn legacy_descriptors(trust: aterm_secrets::commons::journal::Trust) -> Vec<Descriptor> {
    (1..=LEGACY_GENERATIONS)
        .filter_map(|generation| legacy_descriptor_in(trust, generation).ok())
        .collect()
}

/// How long after its seal an old build's claims rows still count (`legacy_active`).
pub(crate) const LEGACY_GRACE_S: u64 = 2 * EPOCH_S;

/// What a read of the table gives.
#[derive(Debug, Clone)]
pub struct Read {
    /// This Mac's Commons machine.
    pub me: [u8; 16],
    /// Each current member's latest row: its machine and the row's body. A member the swarm
    /// roster denies, and a seat of this home's own earlier writer generation
    /// (`former_seats`), has none here.
    pub rows: Vec<([u8; 16], Value)>,
    /// The body of the newest row this home's own earlier writer generations signed: that of
    /// the latest seat in the lineage that holds one (`former_seats`). It is this Mac's past,
    /// not a peer's claim, so it is not among `rows`. A claim in it began this Mac's
    /// uninterrupted use, so where the current seat has no row yet a reader keeps the `since` of
    /// each claim it still uses in that seat's first row, its counter still starting at 1.
    /// None for a home that succeeds none, or whose earlier seats hold no row.
    pub former: Option<Value>,
    /// Whether this Mac may write its row, and why not.
    pub writable: std::result::Result<(), String>,
    /// What this Mac's peers say of its clock, and the peers it reads as skewed
    /// (docs/DESIGN-fluid-swarm-2026-10-28.md F22): under Off, `writable` carries its refusal.
    pub clock: crate::clock::Judgement,
}

/// What this Mac's peers say of its clock at `now` (seconds), from the Commons runtime's
/// `clock.state` (F22).
fn clock_of(
    bootstrap: &Bootstrap,
    bindings: &[aterm_secrets::commons::binding::Binding],
    me: &[u8; 16],
    now: u64,
) -> crate::clock::Judgement {
    crate::clock::judge(bootstrap, bindings, me, now.saturating_mul(1000))
}

fn request_for(key: &str, bytes: &[u8]) -> [u8; 16] {
    let mut sha = Sha256::new();
    sha.update(COMMAND_DOMAIN);
    sha.update(key.as_bytes());
    sha.update([0]);
    sha.update(bytes);
    let digest = sha.finalize();
    let mut request = [0; 16];
    request.copy_from_slice(&digest[..16]);
    request
}

fn command(machine: &[u8; 16], body: &Value) -> Result<Command> {
    let bytes = canonical(body)?;
    let key = hex::encode_lower(machine);
    let request = request_for(&key, &bytes);
    Ok(Command::Write {
        request,
        changes: vec![Change {
            collection: COLLECTION.to_owned(),
            key,
            value: Some(body.clone()),
        }],
    })
}

fn canonical(body: &Value) -> Result<Vec<u8>> {
    if !body.is_object() {
        return Err(Error::new("claim row is not an object"));
    }
    let bytes =
        aterm_json::to_vec(body).map_err(|_| Error::new("claim row could not be encoded"))?;
    if bytes.len() > MAX_ROW {
        return Err(Error::new("claim row exceeds its transport byte budget"));
    }
    Ok(bytes)
}

/// The row fields a decoded command carries (`machine`, `body`, `key`, `request`).
fn row_fields(command: &Command) -> Result<([u8; 16], &Value, &str, &[u8; 16])> {
    let Command::Write { request, changes } = command else {
        return Err(Error::new("claim command is not a write"));
    };
    let [change] = changes.as_slice() else {
        return Err(Error::new("claim command carries one row"));
    };
    let Some(body) = change
        .value
        .as_ref()
        .filter(|_| change.collection == COLLECTION)
    else {
        return Err(Error::new("claim command is not a row"));
    };
    let machine = crate::database::decode_hex::<16>(&change.key)?;
    Ok((machine, body, &change.key, request))
}

/// The row a command carries: its key (the machine it claims to be) and its body.
fn row_of(command: &Command) -> Result<([u8; 16], &Value)> {
    let (machine, body, key, request) = row_fields(command)?;
    if request_for(key, &canonical(body)?) != *request {
        return Err(Error::new(
            "claim command's retry identity is not its row's",
        ));
    }
    Ok((machine, body))
}

fn encode(command: &Command) -> Result<Vec<u8>> {
    let payload = command.encode()?;
    if decode(&payload)? != *command {
        return Err(Error::new("claim encoding would change its meaning"));
    }
    Ok(payload)
}

fn decode(payload: &[u8]) -> Result<Command> {
    if payload.len() > MAX_ROW * 2 {
        return Err(Error::new("claim row exceeds payload limit"));
    }
    let command = Command::decode(payload)?;
    if command.encode()? != payload {
        return Err(Error::new("claim row encoding is not canonical"));
    }
    row_of(&command)?;
    Ok(command)
}

/// Each current member's machine, by its writer actor.
fn machines_of(bindings: &[aterm_secrets::commons::binding::Binding]) -> BTreeMap<Actor, [u8; 16]> {
    bindings
        .iter()
        .map(|binding| (binding.actor(), *binding.machine()))
        .collect()
}

#[cfg(test)]
fn machines(session: &Session<'_>) -> Result<BTreeMap<Actor, [u8; 16]>> {
    Ok(machines_of(&session.members()?))
}

use crate::table::binding_denied_in_roster;

/// A writer's latest row and the table it was read from.
struct Latest {
    source: Source,
    machine: [u8; 16],
    body: Value,
}

/// Each writer's latest row whose key is that writer's own machine, ranked by table, then
/// counter, then sequence.
fn latest(logs: &[(Source, Log)], machines: &BTreeMap<Actor, [u8; 16]>) -> BTreeMap<Actor, Latest> {
    struct Held<'a> {
        rank: (u64, u64, u32, u64),
        source: Source,
        machine: [u8; 16],
        body: &'a Value,
    }
    let mut latest: BTreeMap<Actor, Held<'_>> = BTreeMap::new();
    for (source, log) in logs {
        let (tier, generation) = source.tier();
        for (operation, command) in log.eligible_iter() {
            let actor = operation.header.actor;
            let Some(&expected_machine) = machines.get(&actor) else {
                continue;
            };
            let Ok((machine, body, _, _)) = row_fields(command) else {
                continue;
            };
            if machine != expected_machine {
                continue;
            }
            let counter = body.get("counter").and_then(Value::as_u64).unwrap_or(0);
            let rank = (tier, counter, generation, operation.header.sequence);
            if latest.get(&actor).is_none_or(|held| held.rank < rank) {
                latest.insert(
                    actor,
                    Held {
                        rank,
                        source: *source,
                        machine,
                        body,
                    },
                );
            }
        }
    }
    latest
        .into_iter()
        .map(|(actor, held)| {
            (
                actor,
                Latest {
                    source: held.source,
                    machine: held.machine,
                    body: held.body.clone(),
                },
            )
        })
        .collect()
}

fn me(session: &Session<'_>, machines: &BTreeMap<Actor, [u8; 16]>) -> Result<[u8; 16]> {
    machines
        .get(&session.actor())
        .copied()
        .ok_or_else(|| Error::new("this Mac is not a current Commons member"))
}

fn now() -> Result<u64> {
    aterm_time::unix_millis()
        .map(|ms| ms / 1000)
        .ok_or_else(|| Error::new("system clock cannot place the claims tables"))
}

/// When a row's claims lapse (its `until`, seconds since the Unix epoch); none or a negative one
/// reads as the epoch, which places nothing.
fn until_of(body: &Value) -> u64 {
    body.get("until")
        .and_then(Value::as_i64)
        .and_then(|until| u64::try_from(until).ok())
        .unwrap_or(0)
}

/// The window's day tables this Mac holds, read without enrolling any. A table whose enrollment
/// was cut short after its pending claim holds no row and reads as absent
/// (`Store::observe_if_exists`); the next write to it or repair round of it resumes the join.
fn ring_logs(
    session: &Session<'_>,
    directory: &Directory,
    state: &RingState,
    now: u64,
) -> Result<Vec<(Source, Log)>> {
    let mut logs = Vec::new();
    for epoch in state.window(&CLAIMS, now) {
        let scope = CLAIMS.scope(directory.trust(), epoch)?;
        if let Some(store) = Store::observe_if_exists(session, &directory.path(scope)?, scope)? {
            logs.push((
                Source::Ring(epoch),
                Log::read_with(session, &store, scope, decode)?,
            ));
        }
    }
    Ok(logs)
}

/// Whether the tables an F19 build from before the ring wrote still count: `first`
/// (`accounts/claims/1`) is declared, and is still subscribed or was sealed less than two days
/// before `now` by the sealer's clock. A row an old build wrote just before it heard the seal
/// lapses within a day of it.
fn legacy_active(snapshot: &CatalogSnapshot, first: &Descriptor, now: u64) -> bool {
    let Some(entry) = snapshot.entries.get(&first.scope.digest()) else {
        return false;
    };
    if !entry.declarations.iter().any(|d| d.descriptor == *first) {
        return false;
    }
    if entry.state == State::Subscribed {
        return true;
    }
    entry
        .declarations
        .iter()
        .filter(|d| d.descriptor.is_twin_of(first))
        .map(|d| d.authored_ms)
        .max()
        .is_some_and(|sealed_ms| {
            now.saturating_mul(1000) < sealed_ms.saturating_add(LEGACY_GRACE_S * 1000)
        })
}

/// The old build's tables while they count. They are a courtesy for one grace: one that cannot be
/// read is left out, never an error.
fn legacy_logs(
    session: &Session<'_>,
    directory: &Directory,
    bootstrap: &Bootstrap,
    now: u64,
) -> Vec<(Source, Log)> {
    legacy_logs_from(
        session,
        directory,
        bootstrap,
        directory.snapshot_if_exists(session).ok().as_ref(),
        now,
    )
}

fn legacy_logs_from(
    session: &Session<'_>,
    directory: &Directory,
    bootstrap: &Bootstrap,
    snapshot: Option<&CatalogSnapshot>,
    now: u64,
) -> Vec<(Source, Log)> {
    let (Some(snapshot), Ok(first)) = (snapshot, legacy_descriptor(bootstrap, 1)) else {
        return Vec::new();
    };
    if !legacy_active(snapshot, &first, now) {
        return Vec::new();
    }
    (1..=LEGACY_GENERATIONS)
        .filter_map(|generation| {
            let descriptor = legacy_descriptor(bootstrap, generation).ok()?;
            snapshot.entries.get(&descriptor.scope.digest())?;
            let path = directory.path(descriptor.scope).ok()?;
            let store = Store::observe_if_exists(session, &path, descriptor.scope).ok()??;
            let log = Log::read_with(session, &store, descriptor.scope, decode).ok()?;
            Some((Source::Legacy(generation), log))
        })
        .collect()
}

/// Whether this writer has no room left in the day table `epoch` of `logs`.
fn full(logs: &[(Source, Log)], epoch: u64, caps: Caps, payload: usize) -> bool {
    let (used, needed) = logs
        .iter()
        .find(|(source, _)| *source == Source::Ring(epoch))
        .map_or((Used::default(), 1), |(_, log)| {
            (log.local_use(), log.append_records_bound())
        });
    caps.shares().full(
        &used,
        needed,
        needed.saturating_mul(ring::record_bytes(payload)),
    )
}

/// The day table a row that lapses at `until` goes into: the ring placement (`ring::place`) on the
/// claims ring, whose period is a UTC day, so `until < (epoch + 2) · EPOCH_S` and the row has
/// lapsed on any clock that deletes its table. A full table spills into the next day's while that
/// lies in the window, and no row is written while the clock is behind the floor.
///
/// # Errors
/// The clock is behind the floor, the row lapses beyond the window's last day, or both its tables
/// lack room for the complete append, including its causal joins.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbClaimRing",
        action = "Write",
        project = "aterm_db::accounts::claims::tests::ring_transition"
    )
)]
pub(crate) fn target(
    state: &RingState,
    now: u64,
    own: Option<u64>,
    until: u64,
    full: impl Fn(u64) -> bool,
) -> Result<u64> {
    ring::place(&CLAIMS, state, now, own, until, full).map_err(|unplaced| match unplaced {
        Unplaced::Behind => Error::new(
            "this Mac's clock is behind the claims tables it took up; claims resume when it reaches them",
        ),
        Unplaced::Beyond => Error::new("claim row lapses beyond the claims tables' window"),
        Unplaced::Full => {
            Error::new("account claims are at today's capacity; they resume at 00:00 UTC")
        }
    })
}

/// Every current member's latest row, read without the table's writer lock or enrolling any
/// table: the window's day tables this Mac holds, and an F19 build's earlier table while it counts.
///
/// # Errors
/// This Mac has no Commons or is not a member, or a day table it holds cannot be read.
pub fn read(paths: &Paths) -> Result<Read> {
    read_at(paths, now()?, Caps::default())
}

pub(crate) fn read_at(paths: &Paths, now: u64, caps: Caps) -> Result<Read> {
    read_started(paths, now, caps, Instant::now())
}

fn read_started(paths: &Paths, now: u64, caps: Caps, started: Instant) -> Result<Read> {
    let deadline = crate::admission::deadline_at(started);
    let selected = crate::generation::completed_generation_until(paths, deadline)?;
    let paths = &selected;
    let bootstrap = Bootstrap::required_until(paths, deadline).map_err(crate::table::failure)?;
    let session = Session::observe_named_until(&bootstrap, deadline)?;
    let former = former_seats(&session);
    let directory = Directory::new(paths, bootstrap.trust());
    let state = directory.ring_view(&session, &CLAIMS);
    let mut logs = ring_logs(&session, &directory, &state, now)?;
    let ring = logs.len();
    logs.extend(legacy_logs(&session, &directory, &bootstrap, now));
    let bindings = session.members()?;
    let machines = machines_of(&bindings);
    let latest = latest(&logs, &machines);
    let me = me(&session, &machines)?;
    let clock = clock_of(&bootstrap, &bindings, &me, now);
    let roster = aterm_secrets::client::swarm_roster_until(paths, deadline)
        .map_err(crate::table::failure)?;
    let local_denied = aterm_secrets::client::local_recipient_denied_in_roster(paths)
        || roster.as_ref().is_some_and(|r| {
            bindings
                .iter()
                .find(|b| b.actor() == session.actor())
                .is_some_and(|b| binding_denied_in_roster(r, b))
        });
    let writable = if local_denied {
        Err("this Mac is denied in the swarm roster".to_owned())
    } else if let Some(why) = clock.refusal() {
        Err(why)
    } else {
        session
            .may_write()
            .and_then(|()| {
                for (_, log) in &logs[..ring] {
                    log.writable(session.actor())?;
                }
                target(
                    &state,
                    now,
                    own_epoch(latest.get(&session.actor())),
                    0,
                    |epoch| full(&logs[..ring], epoch, caps, 0),
                )
                .map(drop)
            })
            .map_err(|why| why.to_string())
    };
    let former_row = former
        .iter()
        .rev()
        .find_map(|actor| latest.get(actor))
        .map(|latest| latest.body.clone());
    let denied_actors: std::collections::BTreeSet<Actor> = match &roster {
        Some(r) => bindings
            .iter()
            .filter(|b| b.actor() != session.actor() && binding_denied_in_roster(r, b))
            .map(aterm_secrets::commons::binding::Binding::actor)
            .collect(),
        None => std::collections::BTreeSet::new(),
    };
    Ok(Read {
        me,
        rows: latest
            .into_iter()
            .filter(|(actor, _)| !denied_actors.contains(actor) && !former.contains(actor))
            .map(|(_, latest)| (latest.machine, latest.body))
            .collect(),
        former: former_row,
        writable,
        clock,
    })
}

/// The writer actors of this home's own earlier generations, oldest first: the seats a writer
/// succession left behind on this disk, each still a Commons member, as the generation
/// observation `session` opened with proved them ([`Session::succeeded_writers`]), so the read
/// observes no predecessor twice. [`read`] leaves their rows out of [`Read::rows`] and gives the
/// newest apart ([`Read::former`]). None for a home that succeeds none.
///
/// A former seat's row is this Mac's past, not its claim set: its writer is gone, so nobody
/// renews or releases it, and a peer that stops hearing its machine lapses its leases once the
/// harness's presence window has passed. Taken as a peer's here, it held this Mac's own home
/// accounts for that other machine; taken as this Mac's own written row, its counter would
/// refuse the current seat's first row (which starts at 1). What it does carry is when this
/// Mac's use of each account began, which the current seat's first row keeps.
fn former_seats(session: &Session<'_>) -> Vec<Actor> {
    session
        .succeeded_writers()
        .map(aterm_secrets::commons::binding::Binding::actor)
        .filter(|actor| *actor != session.actor())
        .collect()
}

/// Hand this Mac's latest row to its peers now through `pusher`, this process's Commons runtime
/// ([`crate::runtime::Pusher::push_until`]): a neighbour's acknowledgement on the mesh's relay
/// reaches it at once ([`crate::runtime::Pusher::relay`]); otherwise each round the runtime
/// begins repairs the day table that holds the row first, until a round that began after the ask
/// caught a peer up on it, or `deadline`.
/// What a Mac does as it goes to sleep or stops, when no later round of its own may come.
///
/// # Errors
/// This Mac has no Commons or is not a member, a day table it holds cannot be read, no ring
/// table holds a row of its, or its peers read its clock as off (F22): it then writes no row, and
/// hands none on.
pub fn push(
    paths: &Paths,
    pusher: &crate::runtime::Pusher,
    deadline: Instant,
) -> Result<crate::runtime::Pushed> {
    let table = own_table(paths, now()?, deadline)?;
    Ok(pusher.push_until(table, deadline))
}

/// The day table that holds this writer's latest ring row (its scope digest), read without the
/// table's writer lock or enrolling any table; none while its peers read its clock as off.
fn own_table(paths: &Paths, now: u64, deadline: Instant) -> Result<astream_wire::sync::Digest> {
    own_table_started(paths, now, deadline, Instant::now())
}

fn own_table_started(
    paths: &Paths,
    now: u64,
    deadline: Instant,
    started: Instant,
) -> Result<astream_wire::sync::Digest> {
    let admission = deadline.min(crate::admission::deadline_at(started));
    let bootstrap = Bootstrap::required_until(paths, admission).map_err(crate::table::failure)?;
    let session = Session::observe(&bootstrap)?;
    let directory = Directory::new(paths, bootstrap.trust());
    let bindings = session.members()?;
    let machines = machines_of(&bindings);
    if let Some(why) = clock_of(&bootstrap, &bindings, &me(&session, &machines)?, now).refusal() {
        return Err(Error::new(&why));
    }
    let state = directory.ring_view(&session, &CLAIMS);
    let logs = ring_logs(&session, &directory, &state, now)?;
    let latest = latest(&logs, &machines);
    let epoch = own_epoch(latest.get(&session.actor()))
        .ok_or_else(|| Error::new("no claims table holds this Mac's row"))?;
    Ok(CLAIMS.scope(directory.trust(), epoch)?.digest())
}

/// The day of this writer's latest ring row.
fn own_epoch(latest: Option<&Latest>) -> Option<u64> {
    match latest?.source {
        Source::Ring(epoch) => Some(epoch),
        Source::Legacy(_) => None,
    }
}

/// Write this Mac's row `body` into its day table.
///
/// # Errors
/// The body is not an object of at most [`MAX_ROW`] bytes, this Mac is not a current Commons
/// writer, its own history in a window table is refused, its `until` lies beyond the window, or
/// today's and tomorrow's tables are both full for it. A fresh body's positive `counter` must be 1
/// initially or the current row's counter plus 1; a retry must still be the current row. A stale
/// client must read again before replacing the whole claim set.
pub fn write(paths: &Paths, body: &Value) -> Result<()> {
    write_at(paths, body, now()?, Caps::default())
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbClaimPublication",
        action = "Initialize",
        project = "aterm_db::accounts::claims::tests::publication_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbClaimPublication",
        action = "Retry",
        project = "aterm_db::accounts::claims::tests::publication_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbClaimPublication",
        action = "Stale",
        project = "aterm_db::accounts::claims::tests::publication_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbClaimPublication",
        action = "Advance",
        project = "aterm_db::accounts::claims::tests::publication_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbClaimPublication",
        action = "SupersededRetry",
        project = "aterm_db::accounts::claims::tests::publication_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbClaimPublication",
        action = "Skip",
        project = "aterm_db::accounts::claims::tests::publication_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbClaimClock",
        action = "Write",
        project = "aterm_db::clock::tests::claim_clock_transition"
    )
)]
pub(crate) fn write_at(paths: &Paths, body: &Value, now: u64, caps: Caps) -> Result<()> {
    write_started(paths, body, now, caps, Instant::now())
}

fn write_started(
    paths: &Paths,
    body: &Value,
    now: u64,
    caps: Caps,
    started: Instant,
) -> Result<()> {
    let selected = crate::generation::selected(paths)?;
    let paths = &selected;
    let deadline = crate::admission::deadline_at(started);
    let (bootstrap, journal) =
        Bootstrap::required_with_journal_until(paths, deadline).map_err(crate::table::failure)?;
    let session = Session::from_admitted_journal(&bootstrap, journal)?;
    session.writer()?;
    let bindings = session.members()?;
    if aterm_secrets::client::local_recipient_denied_in_roster(paths)
        || aterm_secrets::client::swarm_roster_until(paths, deadline)
            .map_err(crate::table::failure)?
            .is_some_and(|r| {
                bindings
                    .iter()
                    .find(|b| b.actor() == session.actor())
                    .is_some_and(|b| binding_denied_in_roster(&r, b))
            })
    {
        return Err(Error::new("this Mac is denied in the swarm roster"));
    }
    let machines = machines_of(&bindings);
    let machine = me(&session, &machines)?;
    // A Mac its peers read as off writes no row (F22): a row it wrote would land in a table its
    // peers never read, or outrank its later rows once its clock is set right.
    if let Some(why) = clock_of(&bootstrap, &bindings, &machine, now).refusal() {
        return Err(Error::new(&why));
    }
    let command = command(&machine, body)?;
    let proposed_counter = counter(body)?;
    let encoded = encode(&command)?;

    let directory = Directory::new(paths, bootstrap.trust());
    // An F19 build from before the ring finds every table it declared retired: `claims` and each
    // generation it rolled to (`claims.2` .. `claims.6`), so none stays subscribed and takes a
    // catalog slot on a new Mac (lab model design §3.10). Only a generation still declared alone
    // is sealed, once; a seal this Mac cannot make (its creator budget is spent) never fails the
    // write.
    let mut snapshot = directory.snapshot_if_exists(&session).ok();
    let mut sealed_legacy = false;
    if let Some(snapshot) = &snapshot {
        for generation in 1..=LEGACY_GENERATIONS {
            if let Ok(retired) = legacy_descriptor(&bootstrap, generation)
                && snapshot
                    .entries
                    .get(&retired.scope.digest())
                    .is_some_and(|entry| entry.declarations.iter().all(|d| d.descriptor == retired))
                && directory.seal(&session, &retired).is_ok()
            {
                sealed_legacy = true;
            }
        }
    }
    if sealed_legacy {
        snapshot = directory.snapshot_if_exists(&session).ok();
    }
    let state = directory.retire(&session, &CLAIMS, now)?;
    // The registry roll's housekeeping rides the same writer pass as the ring's retirement: this
    // Mac declares each registry generation it compiles that another Mac declared, which is what
    // lets a roll seal the one before (`alab::roll::endorse`), and at most daily each sealed
    // table whose rows lapsed or were carried frees its directory (`catalog::release`). Neither
    // ever fails the write.
    if let Some(snapshot) = &snapshot {
        crate::alab::roll::endorse(&session, &directory, snapshot);
        let _ = directory.release_due(&session, snapshot, now, &crate::alab::roll::covered);
    }

    // The admitted session holds one exclusive control writer through the revision check across
    // every table read and the append, so no other write lands between them.
    let mut logs = ring_logs(&session, &directory, &state, now)?;
    let mut retry = false;
    for (_, log) in &logs {
        log.writable(session.actor())?;
        retry |= log.retry(session.actor(), &command)?.is_some();
    }
    let ring = logs.len();
    logs.extend(legacy_logs_from(
        &session,
        &directory,
        &bootstrap,
        snapshot.as_ref(),
        now,
    ));
    let latest = latest(&logs, &machines);
    let mine = latest.get(&session.actor());
    let current = mine.map(|latest| &latest.body);
    if retry {
        return if current == Some(body) {
            Ok(())
        } else {
            Err(Error::new(
                "claim row was superseded; read its current revision",
            ))
        };
    }
    let next = match current {
        None => 1,
        Some(body) => counter(body)?
            .checked_add(1)
            .ok_or_else(|| Error::new("claim row counter is exhausted"))?,
    };
    if proposed_counter != next {
        return Err(Error::new(
            "claim row counter is not the next revision; read its current revision",
        ));
    }
    let epoch = target(&state, now, own_epoch(mine), until_of(body), |epoch| {
        full(&logs[..ring], epoch, caps, encoded.len())
    })?;
    let mut store = directory.enroll_ring(&session, &CLAIMS, epoch, now)?;
    crate::documents::application::append(&session, &mut store, &encoded)?;
    Ok(())
}

fn counter(body: &Value) -> Result<u64> {
    body.get("counter")
        .and_then(Value::as_u64)
        .filter(|counter| *counter > 0)
        .ok_or_else(|| Error::new("claim row needs a positive counter"))
}

/// The clock the claims ring runs on moves forward a step (`Tick`), is set back no further than
/// the floor (`ClockBack`), or is set back below it, as a clock that ran ahead and came back
/// (`ClockCorrected`): none is code here; each path that reads the clock places the window by
/// `RingState::current`.
#[cfg(any(test, debug_assertions))]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "DbClaimRing",
        action = "Tick",
        reason = "The clock moving forward is the environment: no code chooses it. claim_ring_conforms advances the clock it hands RingState::current, target and the retirement by each step the model takes."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "DbClaimRing",
        action = "ClockBack",
        reason = "A clock set back is the environment: no code chooses it. claim_ring_conforms sets the clock it hands RingState::current, target and the retirement back a day where the model does."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "DbClaimRing",
        action = "ClockCorrected",
        reason = "A clock that comes back from running ahead is the environment: no code chooses it. claim_ring_conforms sets the clock it hands RingState::current, target and the retirement back a day below the floor where the model does."
    )
)]
#[doc(hidden)]
pub fn db_claim_ring_environment() {}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod frontier_tests;

#[cfg(test)]
mod clock_tests;
