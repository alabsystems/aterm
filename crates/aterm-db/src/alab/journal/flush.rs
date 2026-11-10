// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The flusher (design §3.9, §8; step 8): one pass of the Commons owner's journal upkeep. It
//! takes the spool into its outbox, and when a flush is due reads the window's week tables (the
//! peers' `ev` batches into the heard archive, everyone's `use` and `relay` rows into what this
//! Mac relays), packs one record and appends it on this writer's own chain in its fixed share of
//! the week (`Directory::append_journal`, which spills into the next week when the share is
//! full). What cannot go now stays in the outbox, and the reason is kept in the book
//! (`journal: no storage slot`, a full share, a clock behind the floor) for `sum` and
//! `aterm db alab check` to report.

use super::{
    EV, RELAY, Spool, USE,
    book::{self, Book, Planned, Presence, WEEK},
    event::{Event, Pk},
    heard,
    place::Holding,
    relay::Heard,
};
use crate::{
    Error, Result, att,
    catalog::{
        Directory,
        ring::{self, JOURNAL, JOURNAL_SHARES, Kept, RingState, Used},
    },
    database::Command,
    documents::log::Log,
    table::{Session, store::Store},
    typed::{
        self,
        owned::{self, Op},
    },
};
use astream_wire::sync::{Actor, Digest};
use aterm_codec::hex;
use aterm_json::Value;
use aterm_secrets::{client::Paths, commons::bootstrap::Bootstrap};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::Instant,
};

/// What one pass did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Flushed {
    /// Nothing was due.
    Idle,
    /// One record went into week `period`'s table, with `events` events and `rows` rows.
    Wrote {
        period: u64,
        events: usize,
        rows: usize,
    },
    /// The record could not go out now; the outbox keeps it, for this reason.
    Kept(&'static str),
}

/// [`pass_with`] with no holdings report, so no place row changes.
///
/// # Errors
/// As [`pass_with`].
#[cfg(test)]
pub(crate) fn pass(
    paths: &Paths,
    spool: &Spool,
    presence: &dyn Fn() -> Presence,
    off: bool,
    now: u64,
) -> Result<Flushed> {
    pass_with(paths, spool, presence, &|| None, off, now)
}

/// One flusher pass at `now` (Unix seconds); `presence` is read only when a record is made, and
/// `holdings` (what this Mac holds that no preference here names, [`super::place`]) only once a
/// table read is due, so a quiet minute asks the harness nothing; `None` from it leaves every
/// place row as the book holds it. With the journal off (`off`, `[alab] journal = false`, [`super::publishing`]) what the spool
/// and the outbox hold is dropped unpublished and folded into nothing, and the pass writes only
/// the `sum` row that says so: once a week, and once when the journal is turned off or on.
///
/// # Errors
/// The spool, outbox or book cannot be read or written; no Commons writer; or a window table
/// cannot be read or appended to.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AlabJournal",
        action = "FlushCurrent",
        project = "aterm_db::alab::journal::model_tests::journal_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AlabJournal",
        action = "FlushNext",
        project = "aterm_db::alab::journal::model_tests::journal_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AlabJournal",
        action = "FlushKept",
        project = "aterm_db::alab::journal::model_tests::journal_transition"
    )
)]
pub(crate) fn pass_with(
    paths: &Paths,
    spool: &Spool,
    presence: &dyn Fn() -> Presence,
    holdings: &dyn Fn() -> Option<Vec<Holding>>,
    off: bool,
    now: u64,
) -> Result<Flushed> {
    pass_started(paths, spool, presence, holdings, off, now, Instant::now())
}

/// [`pass_with`] with local custody admission's hang detector running from `started`.
pub(super) fn pass_started(
    paths: &Paths,
    spool: &Spool,
    presence: &dyn Fn() -> Presence,
    holdings: &dyn Fn() -> Option<Vec<Holding>>,
    off: bool,
    now: u64,
    started: Instant,
) -> Result<Flushed> {
    let absorbed = spool.absorb()?;
    let mut book = Book::load(spool);
    let held = spool.outbox()?;
    let before = held.len();
    // Off, nothing noted goes out later either: the owner turned publishing off for exactly
    // these events, so turning it on again publishes none of them. The taken file an off pass
    // took is read only by the next pass, so the first pass after the journal is on again drops
    // it too (`took_off`).
    let (held, absorbed) = if off || book.took_off {
        (Vec::new(), Vec::new())
    } else {
        (held, absorbed)
    };
    let (mut outbox, fresh) = book::merge(held, &absorbed, now);
    // An off pass records that it takes the spool before it does, so a crash between the two
    // leaves the mark; an on pass clears it only once the file it dropped is replaced, so a crash
    // there drops what was noted since (never publishes what was taken while off).
    let marked = off && !book.took_off;
    if marked {
        book.took_off = true;
    }
    // A quiet minute writes nothing: the outbox and book change only with new or lapsed events.
    if !fresh.is_empty() || outbox.len() != before || marked {
        book.fold(&fresh);
        spool.save_outbox(&outbox)?;
        book.save(spool)?;
    }
    // Only now, with what it held in the outbox, is the taken file replaced by the spool.
    spool.take()?;
    if !off && book.took_off {
        book.took_off = false;
        book.save(spool)?;
    }
    let period = now / WEEK;
    // Without a table read: nothing can be due while a hold stands (the pace line, a kept
    // append's retry), nor unless the week is new, a sharp event or a batch waits, or the routine
    // interval passed since the last write or the last read that found nothing to write.
    let waiting = outbox.iter().any(Event::sharp) || outbox.len() >= book::BATCH_EVENTS;
    let routine = now
        >= book
            .flushed_at
            .max(book.checked_at)
            .saturating_add(book::ROUTINE_S);
    // Turning the journal off or on restates the `sum` row; off, nothing else is ever due.
    let flip = book.sum_off != off;
    let quiet = !flip && (off || (!waiting && !routine));
    if now < book.held_until || (book.sum_period == Some(period) && quiet) {
        return Ok(Flushed::Idle);
    }

    // Asked before the journal writer is taken: a first look at the lab reads the registry, which
    // takes `commons.lock`, and an admission holds that lock while it waits for this writer, so
    // asking with the writer in hand would be a lock-order cycle only a hang detector ends.
    let held_here = if off { None } else { holdings() };

    let selected = crate::generation::selected(paths)?;
    let paths = &selected;
    let deadline = crate::admission::deadline_at(started);
    let (bootstrap, journal) =
        Bootstrap::required_with_journal_until(paths, deadline).map_err(crate::table::failure)?;
    let session = Session::from_admitted_journal(&bootstrap, journal)?;
    session.writer()?;
    let me = session
        .members()?
        .iter()
        .find(|binding| binding.actor() == session.actor())
        .map(|binding| *binding.machine())
        .ok_or_else(|| Error::new("this Mac is not a current Commons member"))?;
    let directory = Directory::new(paths, bootstrap.trust());
    let Window {
        current,
        ops,
        used,
        state,
    } = window(&session, &directory, now)?;

    let resolve = owned::commons::historical(&session);
    // A lost or torn book knows no batch it archived: the archive's own files, from the window's
    // first week on, say which, so no batch is archived twice.
    if book.heard.is_empty() {
        let first = used.keys().next().copied().unwrap_or(current);
        book.heard = heard::archived(spool.dir(), first.saturating_mul(WEEK))?;
    }
    let WindowRead {
        heard: mut heard_now,
        batches,
        in_window,
        own_n,
        own,
    } = read_window(&resolve, &ops, &me, &book);
    // A lost or torn book restarts at no counter: the highest this writer's rows in the window
    // carry is the floor, so a new `ev` key never repeats one readers already hold (they keep
    // the first entry of a key, and would drop the new batch).
    book.n = book.n.max(own_n);
    // An event one of this Mac's batches in the window already holds is published: a crash
    // between an append and the outbox's save, or a backfill note of a ledger row the hooks noted
    // first, would otherwise publish it twice.
    let unpublished = outbox.len();
    outbox.retain(|event| !own.contains(event));
    if outbox.len() != unpublished {
        spool.save_outbox(&outbox)?;
    }
    relayable(&mut heard_now, &mut book, &outbox, &me, current);
    heard::archive(spool.dir(), now, &batches)?;
    book.heard = in_window;

    let period = current;
    let n = book.n.saturating_add(1);
    // The pace is the week the record lands in: this writer's own latest week where a share
    // spilled into the next one, so a spill never spends that week's share ahead of its line.
    let paced = |payload: usize| {
        ring::landing(&JOURNAL, JOURNAL_SHARES, &state, now, &used, now, payload).is_ok_and(
            |week| {
                let spent = used.get(&week).copied().unwrap_or_default();
                let elapsed = now.saturating_sub(week.saturating_mul(WEEK));
                Book::under_pace(&spent, elapsed, ring::record_bytes(payload))
            },
        )
    };
    // When the line admits a record of `payload` bytes again, never past the next week's start
    // (whose first record goes whatever the pace); no table to land in waits a retry.
    let admits = |payload: usize| {
        let next_week = period.saturating_add(1).saturating_mul(WEEK);
        ring::landing(&JOURNAL, JOURNAL_SHARES, &state, now, &used, now, payload)
            .map_or(now.saturating_add(book::KEPT_RETRY_S), |week| {
                let spent = used.get(&week).copied().unwrap_or_default();
                let start = week.saturating_mul(WEEK);
                Book::paced_at(&spent, ring::record_bytes(payload))
                    .map_or(start.saturating_add(WEEK), |at| start.saturating_add(at))
            })
            .clamp(now.saturating_add(1), next_week.max(now.saturating_add(1)))
    };
    let new_week = book.sum_period != Some(period);
    let under = paced(0);
    if !new_week && !under {
        book.held_until = admits(0);
        book.save(spool)?;
        return Ok(Flushed::Idle);
    }
    if let Some(held) = held_here {
        book.hold(&held, now);
    }
    if !book.due(&outbox, true, now, period, under, off) {
        book.checked_at = now;
        book.save(spool)?;
        return Ok(Flushed::Idle);
    }
    let sum_due = book.sum_period != Some(period) || book.sum_off != off;
    let presence = if sum_due {
        presence()
    } else {
        Presence::default()
    };
    let planned = book.plan(&outbox, &heard_now, &presence, &me, n, period, now, off)?;
    if planned.changes.is_empty() {
        book.checked_at = now;
        book.save(spool)?;
        return Ok(Flushed::Idle);
    }
    let request = typed::request_id(super::APPLICATION, super::ROLE, &planned.changes)?;
    let payload = Command::encode_write(request, &planned.changes)?;
    // The week's first record goes whatever its size; any other keeps to the line in bytes too.
    if !new_week && !paced(payload.len()) {
        book.held_until = admits(payload.len());
        book.save(spool)?;
        return Ok(Flushed::Idle);
    }
    // The counter is spent before the append, so a crash after it never reuses an `ev` key.
    book.n = n;
    book.save(spool)?;
    let appended = directory
        .append_journal(&session, now, now, &payload)?
        .map(|(placed, _)| placed);
    let landed = appended.is_ok();
    let flushed = settle(&mut outbox, &mut book, &planned, n, period, now, appended);
    if landed {
        spool.save_outbox(&outbox)?;
    }
    book.save(spool)?;
    Ok(flushed)
}

/// What this Mac's next record relays, beside what the window's tables say (`heard`): every fact
/// it believed at its last read ([`Book::known`]), relayed or not, carried on even when the weeks
/// that held them are gone (it was away), so a relay chain survives a gap of its relaying Mac and
/// a Mac that stood down from a bucket another Mac covered fills the gap once that Mac goes away;
/// and the uses and sign-ins its `outbox` is about to publish, first read in week `period`, so the
/// record that publishes a use relays it too. Without the outbox, a use reached a relay only once
/// this Mac read its own batch back, a flush or more later: a Mac that went away first left the
/// use in a batch alone, and once that week's table was deleted no table said it (design §11 step
/// 12, `AlabJournal`'s `RelayKeepsLastUse`). The book then keeps what `heard` holds, each fact
/// used in the last 180 days, at most [`book::MAX_KNOWN`] of them, the newest by (time, machine).
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AlabJournal",
        action = "FlushCurrent",
        project = "aterm_db::alab::journal::model_tests::journal_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AlabJournal",
        action = "FlushNext",
        project = "aterm_db::alab::journal::model_tests::journal_transition"
    )
)]
pub(crate) fn relayable(
    heard: &mut Heard,
    book: &mut Book,
    outbox: &[Event],
    me: &[u8; 16],
    period: u64,
) {
    for ((account, kind), entry) in &book.known {
        heard.offer(*account, *kind, *entry);
    }
    for event in outbox {
        heard.read_event(*me, period, event);
    }
    let since = period.saturating_mul(WEEK).saturating_sub(book::CARRY_S);
    let mut kept: Vec<_> = heard
        .newest
        .iter()
        .filter(|(_, entry)| entry.t >= since)
        .map(|(key, entry)| (*key, *entry))
        .collect();
    kept.sort_by_key(|(_, entry)| std::cmp::Reverse((entry.t, entry.m)));
    kept.truncate(book::MAX_KNOWN);
    book.known = kept.into_iter().collect();
}

/// What the outbox and the book keep once the append of `planned`, at counter `n` in week
/// `period`, returned at `now`: landed (in the week it names), the events it carried leave the
/// outbox and the book records its rows; kept, every event stays in the outbox and the book says
/// why and when to try again. Nothing else takes an event out of the outbox but its 30-day lapse
/// (`book::merge`), so a kept record costs latency, never events.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AlabJournal",
        action = "FlushCurrent",
        project = "aterm_db::alab::journal::model_tests::journal_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AlabJournal",
        action = "FlushNext",
        project = "aterm_db::alab::journal::model_tests::journal_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AlabJournal",
        action = "FlushKept",
        project = "aterm_db::alab::journal::model_tests::journal_transition"
    )
)]
pub(crate) fn settle(
    outbox: &mut Vec<Event>,
    book: &mut Book,
    planned: &Planned,
    n: u64,
    period: u64,
    now: u64,
    appended: std::result::Result<u64, Kept>,
) -> Flushed {
    match appended {
        Ok(placed) => {
            outbox.drain(..planned.events);
            book.wrote(planned, n, period, now);
            Flushed::Wrote {
                period: placed,
                events: planned.events,
                rows: planned.changes.len() - usize::from(planned.events > 0),
            }
        }
        Err(kept) => {
            book.kept = Some(kept.reason().to_owned());
            book.held_until = now.saturating_add(book::KEPT_RETRY_S);
            Flushed::Kept(kept.reason())
        }
    }
}

/// The journal window's tables as this Mac holds them at `now`.
pub(crate) struct Window {
    /// The current week.
    pub current: u64,
    /// Every eligible typed operation of the window's tables.
    pub ops: Vec<Op>,
    /// What each window table this Mac holds retains, in all and of this writer's, by week.
    pub used: BTreeMap<u64, Used>,
    /// The ring state the window was read by.
    pub state: RingState,
}

/// Read the journal window's tables at `now`.
///
/// # Errors
/// A table cannot be observed or read.
pub(crate) fn window(session: &Session<'_>, directory: &Directory, now: u64) -> Result<Window> {
    let state = directory.ring_view(session, &JOURNAL);
    let current = state.current(&JOURNAL, now);
    let mut ops = Vec::new();
    let mut used = BTreeMap::new();
    for epoch in state.window(&JOURNAL, now) {
        let scope = JOURNAL.scope(directory.trust(), epoch)?;
        let Some(store) = Store::observe_if_exists(session, &directory.path(scope)?, scope)? else {
            continue;
        };
        let log = Log::read_with(session, &store, scope, att::decode)?;
        used.insert(epoch, log.local_use());
        ops.extend(
            log.into_eligible().filter_map(|(operation, command)| {
                Op::from_eligible_owned(epoch, operation, command)
            }),
        );
    }
    Ok(Window {
        current,
        ops,
        used,
        state,
    })
}

/// What the window's tables say ([`read_window`]).
pub(crate) struct WindowRead {
    /// Each account's newest use and sign-ins, and the buckets other Macs relay.
    pub heard: Heard,
    /// The `ev` batches the heard archive does not hold yet.
    pub batches: Vec<heard::Batch>,
    /// Every batch the window holds, by writer, which the book keeps so the archive takes each
    /// batch once.
    pub in_window: BTreeMap<String, BTreeSet<u64>>,
    /// The highest counter this Mac's own rows carry.
    pub own_n: u64,
    /// Every event this Mac's own batches in the window hold: published already.
    pub own: BTreeSet<Event>,
}

/// What the window's tables say ([`WindowRead`]). Each row's writer is the machine `resolve`
/// names for its record (the historical binding, `owned::commons::historical`).
pub(crate) fn read_window(
    resolve: &dyn Fn(&Digest, &Actor) -> Option<[u8; 16]>,
    ops: &[Op],
    me: &[u8; 16],
    book: &Book,
) -> WindowRead {
    let ev = owned::read_log(&EV, ops, resolve);
    let sums = owned::read_owned(&super::SUM, ops, resolve);
    let uses = owned::read_owned(&USE, ops, resolve);
    let relays = owned::read_owned(&RELAY, ops, resolve);
    let places = owned::read_owned(&super::PLACE, ops, resolve);
    let mut heard = Heard::default();
    let mut batches = Vec::new();
    let mut in_window: BTreeMap<String, BTreeSet<u64>> = BTreeMap::new();
    let mut own = BTreeSet::new();
    let counter = |value: &Value| value.get("n").and_then(Value::as_u64).unwrap_or(0);
    let own_n = ev
        .entries
        .iter()
        .filter(|entry| entry.machine == *me)
        .map(|entry| entry.n)
        .chain(
            sums.rows
                .values()
                .chain(uses.rows.values())
                .chain(relays.rows.values())
                .chain(places.rows.values())
                .filter(|row| row.machine == *me)
                .map(|row| counter(&row.value)),
        )
        .max()
        .unwrap_or(0);
    for entry in &ev.entries {
        let events: Vec<Event> = entry
            .value
            .get("e")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Event::from_value)
            .collect();
        for event in &events {
            heard.read_event(entry.machine, entry.period, event);
        }
        if entry.machine == *me {
            own.extend(events.iter().copied());
        }
        let machine = hex::encode_lower(&entry.machine);
        let archived = book
            .heard
            .get(&machine)
            .is_some_and(|held| held.contains(&entry.n));
        if !archived && !events.is_empty() {
            batches.push((machine.clone(), entry.n, entry.period, events));
        }
        in_window.entry(machine).or_default().insert(entry.n);
    }
    for (key, row) in &uses.rows {
        if let Some(account) = key.split_once('.').and_then(|(pk, _)| Pk::new(pk)) {
            heard.read_use(account, row.machine, row.rank.0, &row.value);
        }
    }
    for (key, row) in &relays.rows {
        if key_relay_bucket(key)
            .is_some_and(|b| row.value.get("b").and_then(Value::as_u64) == Some(u64::from(b)))
        {
            heard.read_relay(row.machine, me, &row.value, row.rank.0);
        }
    }
    WindowRead {
        heard,
        batches,
        in_window,
        own_n,
        own,
    }
}

pub(crate) fn key_relay_bucket(key: &str) -> Option<u8> {
    let (prefix, _) = key.split_once('.')?;
    let hex = prefix.strip_prefix("rl-")?;
    if hex.len() != 1 {
        return None;
    }
    match hex.as_bytes()[0] {
        b @ b'0'..=b'9' => Some(b - b'0'),
        b @ b'a'..=b'f' => Some(b - b'a' + 10),
        _ => None,
    }
}
