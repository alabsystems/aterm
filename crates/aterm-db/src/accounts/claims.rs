// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! ACCOUNT CLAIMS ON THE LAB'S DAY TABLES (docs/DESIGN-fluid-swarm-2026-10-28.md F19): one row per
//! Mac, the whole set of accounts it claims, each signed by the Mac's own Commons writer. A row's
//! key is its signer's machine, so no Mac can write another Mac's row: a row at any other key is
//! inert. The row's body is the harness's (`aterm_agent::harness::claims::Row`); a table holds it
//! whole, at most [`MAX_ROW`] bytes, and gives each writer's latest one back. A fresh row advances
//! its counter exactly once under the table writer, so a stale whole-set replacement cannot erase
//! another local client's claims. Any current Writer writes its row; nothing brokers.
//!
//! Rows ride a ring of UTC-day tables (`crate::catalog::ring`, [`EPOCH_S`]) that no catalog
//! declares, so nothing fills however long the lab runs. A row goes into its writer's day table,
//! or a later table of its own, never earlier than the day before the one its `until` falls in, so
//! every row of a table lapses before the day after next begins; each Mac deletes a day's table
//! then, on its own clock. Of two rows from one Mac the one in the later table wins, then the
//! higher counter. A day table takes [`Caps`] rows, then the next day's. Revision and retry checks
//! cover every table read, under one writer.
//!
//! An F19 build from before the ring wrote `accounts/claims/1`, rolling to `claims.2` through
//! `claims.6`. A ring Mac that finds that declaration declares the same table at version 2, once
//! (`Directory::seal`): the old build's reads and writes then fail, so it keeps to its home
//! accounts. Ring Macs read the old tables, ranked below every ring row, while the old table is
//! still subscribed and for two days after the seal.

use crate::{
    Error, Result,
    catalog::{
        Descriptor, Directory, Snapshot as CatalogSnapshot, State,
        ring::{CLAIMS, RingState},
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
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

/// A row's canonical body is at most this long.
pub const MAX_ROW: usize = 4096;
/// The length of one claims table's period: one table per UTC day. A row written now must lapse
/// within this long (the harness's `HORIZON_S`) to land in the current day's table.
pub const EPOCH_S: u64 = 86_400;
/// The tables an F19 build from before the ring rolled through (`claims` .. `claims.6`), read only
/// while that build's table counts.
const LEGACY_GENERATIONS: u32 = 6;
const COLLECTION: &str = "row";
const COMMAND_DOMAIN: &[u8] = b"aterm/accounts/claims/command/1\0";

/// How many rows one day table takes, in all and from one writer, before a write spills into the
/// next day's: below the store's 8192 records, so the two sides of a split each fill no more than
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
    let role = if generation <= 1 {
        "claims".to_owned()
    } else {
        format!("claims.{generation}")
    };
    Descriptor::application(bootstrap.trust(), "accounts", &role, 1)
}

/// What a read of the table gives.
#[derive(Debug, Clone)]
pub struct Read {
    /// This Mac's Commons machine.
    pub me: [u8; 16],
    /// Each current member's latest row: its machine and the row's body.
    pub rows: Vec<([u8; 16], Value)>,
    /// Whether this Mac may write its row, and why not.
    pub writable: std::result::Result<(), String>,
}

fn command(machine: &[u8; 16], body: &Value) -> Result<Command> {
    let bytes = canonical(body)?;
    let key = hex::encode_lower(machine);
    let mut input = COMMAND_DOMAIN.to_vec();
    input.extend_from_slice(key.as_bytes());
    input.push(0);
    input.extend_from_slice(&bytes);
    let digest = Sha256::digest(&input);
    let mut request = [0; 16];
    request.copy_from_slice(&digest[..16]);
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
        return Err(Error::new("claim row exceeds 4 KiB"));
    }
    Ok(bytes)
}

/// The row a command carries: its key (the machine it claims to be) and its body.
fn row_of(command: &Command) -> Result<([u8; 16], &Value)> {
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
    if command_request(&machine, body)? != *request {
        return Err(Error::new(
            "claim command's retry identity is not its row's",
        ));
    }
    Ok((machine, body))
}

fn command_request(machine: &[u8; 16], body: &Value) -> Result<[u8; 16]> {
    Ok(command(machine, body)?.request())
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
fn machines(session: &Session<'_>) -> BTreeMap<Actor, [u8; 16]> {
    session
        .members()
        .iter()
        .map(|binding| (binding.actor(), *binding.machine()))
        .collect()
}

/// A writer's latest row and the table it was read from.
struct Latest {
    rank: (u64, u64, u32, u64),
    source: Source,
    machine: [u8; 16],
    body: Value,
}

/// Each writer's latest row whose key is that writer's own machine, ranked by table, then
/// counter, then sequence.
fn latest(logs: &[(Source, Log)], machines: &BTreeMap<Actor, [u8; 16]>) -> BTreeMap<Actor, Latest> {
    let mut latest: BTreeMap<Actor, Latest> = BTreeMap::new();
    for (source, log) in logs {
        let (tier, generation) = source.tier();
        for (operation, command) in log.eligible() {
            let actor = operation.header.actor;
            let Ok((machine, body)) = row_of(&command) else {
                continue;
            };
            if machines.get(&actor) != Some(&machine) {
                continue;
            }
            let counter = body.get("counter").and_then(Value::as_u64).unwrap_or(0);
            let rank = (tier, counter, generation, operation.header.sequence);
            if latest.get(&actor).is_none_or(|held| held.rank < rank) {
                latest.insert(
                    actor,
                    Latest {
                        rank,
                        source: *source,
                        machine,
                        body: body.clone(),
                    },
                );
            }
        }
    }
    latest
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
    let twin = Descriptor {
        version: first.version.saturating_add(1),
        ..first.clone()
    };
    entry
        .declarations
        .iter()
        .filter(|d| d.descriptor == twin)
        .map(|d| d.authored_ms)
        .max()
        .is_some_and(|sealed_ms| {
            now.saturating_mul(1000) < sealed_ms.saturating_add(2 * EPOCH_S * 1000)
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
    let (Ok(snapshot), Ok(first)) = (
        directory.snapshot_if_exists(session),
        legacy_descriptor(bootstrap, 1),
    ) else {
        return Vec::new();
    };
    if !legacy_active(&snapshot, &first, now) {
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
fn full(logs: &[(Source, Log)], epoch: u64, actor: Actor, caps: Caps) -> bool {
    logs.iter()
        .find(|(source, _)| *source == Source::Ring(epoch))
        .is_some_and(|(_, log)| {
            let (records, writer_records) = log.retained_counts(actor);
            records >= caps.records || writer_records >= caps.writer_records
        })
}

/// The day table a row that lapses at `until` goes into, by the clock `now` and this Mac's ring
/// `state`: the current day's, this writer's own latest day's (`own`) where later, never below the
/// floor, so never a table this Mac deleted (`RingState::outdated`), and never earlier than the
/// day before the one `until` falls in, so `until < (epoch + 2) · EPOCH_S` and the row has lapsed
/// on any clock that deletes its table. A full table spills into the next day's while that lies
/// in the window.
///
/// No row is written while the clock is behind the floor. Such a clock either ran ahead and came
/// back or is set back, and no Mac can tell which: set back, its window (the clock's,
/// `RingState::current`) leaves out the day after the floor, its peers' today, where its own
/// latest row and its peers' current claims can sit, so a row written at the floor would be
/// claimed on a view without them and outranked by its own later one. Once the clock reaches the
/// floor, the window reaches every table that holds a row of this Mac's: each write raises the
/// floor to the day before the current one first and goes at most a day past the current one, so
/// no row lies more than two days past the floor.
///
/// # Errors
/// The clock is behind the floor, the row lapses beyond the window's last day, or both its tables
/// are full.
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
    let last = *state.window(&CLAIMS, now).end();
    let epoch = state
        .current(&CLAIMS, now)
        .max(state.floor)
        .max(own.unwrap_or(0))
        .max(CLAIMS.epoch_of(until).saturating_sub(1));
    if CLAIMS.epoch_of(now) < state.floor {
        return Err(Error::new(
            "this Mac's clock is behind the claims tables it took up; claims resume when it reaches them",
        ));
    }
    if epoch > last {
        return Err(Error::new(
            "claim row lapses beyond the claims tables' window",
        ));
    }
    (epoch..=last).find(|epoch| !full(*epoch)).ok_or_else(|| {
        Error::new("account claims are at today's capacity; they resume at 00:00 UTC")
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

fn read_at(paths: &Paths, now: u64, caps: Caps) -> Result<Read> {
    let deadline = Instant::now() + Duration::from_secs(2);
    let bootstrap = Bootstrap::required_until(paths, deadline).map_err(crate::table::failure)?;
    let session = Session::observe(&bootstrap)?;
    let directory = Directory::new(paths, bootstrap.trust());
    let state = directory.ring_view(&session, &CLAIMS);
    let mut logs = ring_logs(&session, &directory, &state, now)?;
    let ring = logs.len();
    logs.extend(legacy_logs(&session, &directory, &bootstrap, now));
    let machines = machines(&session);
    let latest = latest(&logs, &machines);
    let writable = session
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
                |epoch| full(&logs[..ring], epoch, session.actor(), caps),
            )
            .map(drop)
        })
        .map_err(|why| why.to_string());
    Ok(Read {
        me: me(&session, &machines)?,
        rows: latest
            .into_values()
            .map(|latest| (latest.machine, latest.body))
            .collect(),
        writable,
    })
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
fn write_at(paths: &Paths, body: &Value, now: u64, caps: Caps) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(2);
    let (bootstrap, journal) =
        Bootstrap::required_with_journal_until(paths, deadline).map_err(crate::table::failure)?;
    let session = Session::from_admitted_journal(&bootstrap, journal)?;
    session.writer()?;
    let machines = machines(&session);
    let machine = me(&session, &machines)?;
    let command = command(&machine, body)?;
    let proposed_counter = counter(body)?;
    let encoded = encode(&command)?;

    let directory = Directory::new(paths, bootstrap.trust());
    // An F19 build from before the ring finds its table retired; a seal this Mac cannot make
    // (its creator budget is spent) never fails the write.
    if let Ok(first) = legacy_descriptor(&bootstrap, 1) {
        let _ = directory.seal(&session, &first);
    }
    let state = directory.retire(&session, &CLAIMS, now)?;

    // The admitted session holds one exclusive control writer through the revision check across
    // every table read and the append, so no other write lands between them.
    let mut logs = ring_logs(&session, &directory, &state, now)?;
    let mut retry = false;
    for (_, log) in &logs {
        log.writable(session.actor())?;
        retry |= log.retry(session.actor(), &command)?.is_some();
    }
    let ring = logs.len();
    logs.extend(legacy_logs(&session, &directory, &bootstrap, now));
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
        full(&logs[..ring], epoch, session.actor(), caps)
    })?;
    let mut store = directory.enroll_ring(&session, &CLAIMS, epoch, now)?;
    store.commit_catalog(&session, &encoded)?;
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
