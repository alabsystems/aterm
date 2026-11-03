// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What this Mac's flusher knows and has said (design §4.17, §3.9): per account it used, the
//! facts its `use` row states (first and last use, sessions, sign-in state, the latest reading
//! and wall per window, the plan); the counter its records' keys and rows carry; what it last
//! wrote of each row and when; and the pace that keeps its records within its fixed share of a
//! week's table. Pure: `super::flush` loads it from `<spool>/book.json`, hands it the window's
//! tables and saves it.

use super::{
    event::{Event, Kind, MAX_EVENT, Pk, Window, Word},
    relay::{Entry, Heard, Rk},
    spool::{KEEP_S, MAX_SPOOL},
};
use crate::{
    Error, Result,
    catalog::ring::{JOURNAL_SHARES, Used},
    database::{Change, Command, MAX_WRITES},
    typed::owned::{caps, fit},
};
use aterm_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet};

/// One week, the journal ring's period.
pub(crate) const WEEK: u64 = crate::catalog::ring::JOURNAL.period_s;
const DAY: u64 = 86_400;
/// A changed `use` row is restated at most this often (design §4.17).
pub(crate) const USE_EVERY: u64 = 3600;
/// An account used within this long has its `use` row carried into every new week, so it
/// outlives the deletion of the weeks that held it (design §4.17).
pub(crate) const CARRY_S: u64 = 180 * DAY;
/// With no sharp event waiting, a flush at most this often while something waits (design §3.9).
pub(crate) const ROUTINE_S: u64 = 6 * 3600;
/// ... or as soon as this many events wait.
pub(crate) const BATCH_EVENTS: usize = 128;
/// After a kept append (no slot, a full share, a clock behind the floor), the flusher reads
/// no table again for this long, the latency the design allows a sharp event (§3.9).
pub(crate) const KEPT_RETRY_S: u64 = 20 * 60;
/// Records of a week a Mac may spend at once before the pace line binds: the line then grows
/// evenly to its whole share by the week's end, so a busy Mac's flushes stretch out and its
/// share is never spent early (design §3.9: the cap costs latency, never events).
pub(crate) const BURST: usize = 8;
/// The schema version every journal row carries.
pub(crate) const VERSION: u64 = 1;
/// Session starts a `use` row counts back over (`sessions_7d`).
const MAX_STARTS: usize = 64;
/// Accounts one book keeps; the least recently used go first.
const MAX_ACCOUNTS: usize = 512;
/// Facts one book keeps in [`Book::known`]: each kept account's three relay kinds. At about 100
/// bytes of JSON a fact, they stay far inside [`MAX_BOOK`].
pub(crate) const MAX_KNOWN: usize = 3 * MAX_ACCOUNTS;
/// The book's file in the spool folder.
pub(crate) const BOOK: &str = "book.json";
const MAX_BOOK: u64 = 8 * 1024 * 1024;

/// A `use` row's sign-in state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SignIn {
    /// `in`, `out`, `ended`, `wrong` or `failed`.
    pub state: &'static str,
    pub t: u64,
    pub cli: String,
}

const SIGNIN_STATES: [&str; 5] = ["in", "out", "ended", "wrong", "failed"];

/// What a row last said and when, so an unchanged row is never written again within a week.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Stated {
    pub value: Value,
    pub at: u64,
    pub period: u64,
}

/// What this Mac knows of one account it used, or of one provider workspace of it (a
/// sub-account, design §4.13), each its own `use` row.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Facts {
    pub first: u64,
    pub last_start: u64,
    pub last_end: u64,
    /// Session starts, newest last, at most [`MAX_STARTS`].
    pub starts: Vec<u64>,
    pub signin: Option<SignIn>,
    /// The latest reading of each window.
    pub quota: BTreeMap<Window, Event>,
    /// Each window's standing wall: until, and when it was seen.
    pub walls: BTreeMap<Window, (Option<u64>, u64)>,
    pub plan: Option<(String, u64)>,
    /// On a sub-account's row, the provider's `AccountKey` it is keyed by (12 hex digits).
    pub key: Option<String>,
    /// On a sub-account's row, the account it belongs to: a sub-account key is a one-way digest
    /// of the two, so a reader joins the rows by this column and checks the key against both.
    pub account: Option<Pk>,
    /// The newest event about it.
    pub used: u64,
    pub stated: Option<Stated>,
}

impl Facts {
    /// On an account's row, give up the window a workspace's reading or wall `e` (one naming
    /// `s`) now states on the sub-account's row: an older reading or wall of that window held
    /// here (one folded before readings named their workspace) would otherwise be restated
    /// beside the live one each week for as long as the account is used.
    pub(crate) fn yield_window(&mut self, e: &Event) {
        let Some(w) = e.window else { return };
        match e.kind {
            Kind::Quota if self.quota.get(&w).is_some_and(|held| held.t <= e.t) => {
                self.quota.remove(&w);
            }
            Kind::Wall | Kind::WallCleared
                if self.walls.get(&w).is_some_and(|(_, t)| *t <= e.t) =>
            {
                self.walls.remove(&w);
            }
            _ => {}
        }
    }

    /// Fold one event in: newer facts replace older ones, never the other way.
    pub(crate) fn fold(&mut self, e: &Event) {
        if self.first == 0 || e.t < self.first {
            self.first = e.t;
        }
        self.used = self.used.max(e.t);
        let signin = |state: &'static str, held: &mut Option<SignIn>| {
            if held.as_ref().is_none_or(|s| s.t <= e.t) {
                *held = Some(SignIn {
                    state,
                    t: e.t,
                    cli: e.cli.as_str().to_owned(),
                });
            }
        };
        match e.kind {
            Kind::SessionStart => {
                self.last_start = self.last_start.max(e.t);
                if let Err(at) = self.starts.binary_search(&e.t) {
                    self.starts.insert(at, e.t);
                }
                if self.starts.len() > MAX_STARTS {
                    self.starts.remove(0);
                }
            }
            Kind::SessionEnd => self.last_end = self.last_end.max(e.t),
            Kind::SignedIn | Kind::Renewed => signin("in", &mut self.signin),
            Kind::SignedOut | Kind::Displaced => signin("out", &mut self.signin),
            Kind::LoginEnded => signin("ended", &mut self.signin),
            Kind::WrongAccount => signin("wrong", &mut self.signin),
            Kind::SignInFailed => signin("failed", &mut self.signin),
            Kind::Quota => {
                if let (Some(w), Some(_)) = (e.window, e.pct_x10)
                    && self.quota.get(&w).is_none_or(|held| held.t <= e.t)
                {
                    self.quota.insert(w, *e);
                }
            }
            Kind::Wall => {
                if let Some(w) = e.window
                    && self.walls.get(&w).is_none_or(|(_, t)| *t <= e.t)
                {
                    self.walls.insert(w, (e.until, e.t));
                }
            }
            Kind::WallCleared => {
                if let Some(w) = e.window
                    && self.walls.get(&w).is_some_and(|(_, t)| *t <= e.t)
                {
                    self.walls.remove(&w);
                }
            }
            Kind::PlanSeen => {
                if let Some(plan) = &e.plan
                    && self.plan.as_ref().is_none_or(|(_, t)| *t <= e.t)
                {
                    self.plan = Some((plan.as_str().to_owned(), e.t));
                }
            }
            _ => {}
        }
    }

    /// The `use` row (design §4.17) at counter `n` in week `period`, within `cap` bytes: over
    /// it, walls whose `until` has passed go first, then the oldest readings, never anything
    /// else (`typed::owned::fit`).
    ///
    /// # Errors
    /// Even the row with no readings and no walls passes `cap`.
    pub(crate) fn use_row(&self, n: u64, period: u64, now: u64, cap: usize) -> Result<Value> {
        let mut base = Map::new();
        base.insert("r".into(), Value::from(VERSION));
        base.insert("n".into(), Value::from(n));
        base.insert("period".into(), Value::from(period));
        base.insert("first".into(), Value::from(self.first));
        base.insert("last_start".into(), Value::from(self.last_start));
        base.insert("last_end".into(), Value::from(self.last_end));
        let recent = self
            .starts
            .iter()
            .filter(|t| t.saturating_add(7 * DAY) > now)
            .count();
        base.insert("sessions_7d".into(), Value::from(recent as u64));
        if let Some(s) = &self.signin {
            base.insert(
                "signin".into(),
                json!({"state": s.state, "t": s.t, "cli": s.cli}),
            );
        }
        if let Some((word, t)) = &self.plan {
            base.insert("plan".into(), json!({"word": word, "t": t}));
        }
        if let Some(key) = &self.key {
            base.insert("key".into(), Value::from(key.as_str()));
        }
        if let Some(account) = &self.account {
            base.insert("account".into(), Value::from(account.as_str()));
        }
        // Priority, most kept first: standing walls, readings newest first, lapsed walls.
        let wall = |w: &Window, until: &Option<u64>, t: &u64| json!({"wall": {"w": w.word(), "until": until, "t": t}});
        let mut standing: Vec<_> = self
            .walls
            .iter()
            .filter(|(_, (until, _))| until.is_none_or(|u| u > now))
            .collect();
        let mut lapsed: Vec<_> = self
            .walls
            .iter()
            .filter(|(_, (until, _))| until.is_some_and(|u| u <= now))
            .collect();
        standing.sort_by_key(|(_, (_, t))| std::cmp::Reverse(*t));
        lapsed.sort_by_key(|(_, (_, t))| std::cmp::Reverse(*t));
        let mut readings: Vec<&Event> = self.quota.values().collect();
        readings.sort_by_key(|e| std::cmp::Reverse(e.t));
        let entries: Vec<Value> = standing
            .into_iter()
            .map(|(w, (u, t))| wall(w, u, t))
            .chain(readings.into_iter().map(|e| {
                json!({"quota": {"w": e.window.map(Window::word), "p": e.pct_x10,
                    "r": e.reset, "rr": e.res.map(|r| r.word()), "t": e.t,
                    "src": e.src.word()}})
            }))
            .chain(lapsed.into_iter().map(|(w, (u, t))| wall(w, u, t)))
            .collect();
        let wrap = |kept: &[Value]| {
            let mut row = base.clone();
            let pick = |key: &str| -> Vec<Value> {
                kept.iter().filter_map(|e| e.get(key).cloned()).collect()
            };
            row.insert("quota".into(), Value::Array(pick("quota")));
            row.insert("wall".into(), Value::Array(pick("wall")));
            Value::Object(row)
        };
        let kept = fit(&entries, cap, wrap)?;
        Ok(wrap(&entries[..kept]))
    }
}

/// What a `use` or `relay` row says, without its counter and week, to compare with what was
/// last written.
pub(crate) fn content(row: &Value) -> Value {
    let mut row = row.as_object().cloned().unwrap_or_default();
    for key in ["n", "period"] {
        row.remove(key);
    }
    Value::Object(row)
}

/// What this Mac states about itself in its `sum` row (design §4.17), read by the caller.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Presence {
    /// Its membership label (`m22`).
    pub label: Option<Word<16>>,
    /// The macOS major version.
    pub os: Option<u64>,
}

/// The flusher's state.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Book {
    pub accounts: BTreeMap<Pk, Facts>,
    /// The last counter a record of this Mac's used: never reused, so an `ev` batch key is new.
    pub n: u64,
    /// The week of the last `sum` row.
    pub sum_period: Option<u64>,
    /// Whether the last `sum` row said `journal: off` (`[alab] journal = false`), so turning the
    /// journal off or on again restates it within the week.
    pub sum_off: bool,
    /// The last pass took the spool with the journal off: the file it took holds what was noted
    /// while off, so the next pass drops it unread whatever the setting is then.
    pub took_off: bool,
    /// Each relayed bucket's last row.
    pub relays: BTreeMap<u8, Stated>,
    pub flushed_at: u64,
    /// The last table read that wrote nothing: the routine interval runs from it as from a
    /// write, so an idle Mac reads its window once in six hours, not once a minute.
    pub checked_at: u64,
    /// No table is read before this: the moment the pace line next admits a record (never past
    /// the next week's start), or a kept append's retry ([`KEPT_RETRY_S`]).
    pub held_until: u64,
    /// Why the last flush was kept local (`catalog::ring::Kept::reason`), until one goes out.
    pub kept: Option<String>,
    /// The `ev` batches already in the heard archive, by writer machine, among those the window's
    /// tables still hold.
    pub heard: BTreeMap<String, BTreeSet<u64>>,
    /// Every fact this Mac believed at its last table read, relayed or not, each account's
    /// newest of each kind used in the last 180 days, at most [`MAX_KNOWN`], the newest kept:
    /// what it restates once the tables that held a fact were deleted, both its own earlier
    /// relays and a bucket another Mac covered that falls to it after that Mac went away (design
    /// §11 step 12).
    pub known: BTreeMap<(Pk, Rk), Entry>,
}

/// One planned flush record (design §4.17 "Record packing").
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Planned {
    pub changes: Vec<Change>,
    /// How many of the outbox's oldest events ride its `ev` batch.
    pub events: usize,
    pub sum: bool,
    pub uses: Vec<(Pk, Value)>,
    pub relays: Vec<(u8, Value)>,
    /// Planned with the journal off: its `sum` says so.
    pub off: bool,
}

/// Whether `changes` fit one flush record: at most 64 changes and 60 KiB of payload.
fn fits(changes: &[Change]) -> bool {
    changes.len() <= MAX_WRITES
        && Command::Write {
            request: [0; 16],
            changes: changes.to_vec(),
        }
        .encode()
        .is_ok_and(|payload| payload.len() <= caps::RECORD)
}

/// `outbox` with `absorbed` added, each event once, oldest first, those older than 30 days and
/// the oldest past the spool's cap dropped; and which of `absorbed` were new.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "AlabJournal",
        action = "Note",
        project = "aterm_db::alab::journal::model_tests::journal_transition"
    )
)]
pub(crate) fn merge(outbox: Vec<Event>, absorbed: &[Event], now: u64) -> (Vec<Event>, Vec<Event>) {
    let mut held: BTreeSet<Event> = outbox.into_iter().collect();
    let fresh: Vec<Event> = absorbed
        .iter()
        .filter(|e| held.insert(**e))
        .copied()
        .collect();
    let mut all: Vec<Event> = held
        .into_iter()
        .filter(|e| e.t.saturating_add(KEEP_S) >= now)
        .collect();
    let mut bytes: u64 = 0;
    let mut keep = all.len();
    let mut buf = [0u8; MAX_EVENT];
    for (i, e) in all.iter().enumerate().rev() {
        bytes += e.encode(&mut buf).map_or(0, |n| n as u64 + 1);
        if bytes > MAX_SPOOL {
            break;
        }
        keep = i;
    }
    all.drain(..keep);
    (all, fresh)
}

/// The `use` row an event folds into (design §4.17, `<account or sub-account pk>.<uuid32>`): an
/// event naming a provider workspace (`s`) is that sub-account's (§4.13, its key derived from the
/// account and the workspace's `AccountKey` with no lookup), so two workspaces of one login keep
/// their own quota; any other is its account's; a realm-only `probe` names no row. With the
/// sub-account's key come the account and the `AccountKey` its row names.
pub(crate) fn row_of(e: &Event) -> Option<(Pk, Option<(Pk, String)>)> {
    let account = e.account?;
    let Some(sub) = &e.sub else {
        return Some((account, None));
    };
    let pk = crate::typed::keys::subaccount_pk(account.as_str(), sub.as_str()).ok()?;
    Some((Pk::new(&pk)?, Some((account, sub.as_str().to_owned()))))
}

impl Book {
    /// Fold events into the accounts' facts, keeping at most [`MAX_ACCOUNTS`] rows.
    pub(crate) fn fold(&mut self, events: &[Event]) {
        for e in events {
            let Some((row, sub)) = row_of(e) else {
                continue;
            };
            if let Some((account, _)) = &sub
                && let Some(parent) = self.accounts.get_mut(account)
            {
                parent.yield_window(e);
            }
            let facts = self.accounts.entry(row).or_default();
            if let Some((account, key)) = sub {
                facts.account = Some(account);
                facts.key = Some(key);
            }
            facts.fold(e);
        }
        while self.accounts.len() > MAX_ACCOUNTS {
            let Some(stalest) = self
                .accounts
                .iter()
                .min_by_key(|(pk, f)| (f.used, **pk))
                .map(|(pk, _)| *pk)
            else {
                break;
            };
            self.accounts.remove(&stalest);
        }
    }

    /// Whether this Mac is under its pace line: having spent `used` of its share of the week
    /// `elapsed` seconds old, one more record of `bytes` wire bytes stays within the line in
    /// records ([`BURST`] plus the share's even spread) and in bytes (the same fraction of its
    /// byte share), so large records keep to the week as small ones do.
    pub(crate) fn under_pace(used: &Used, elapsed: u64, bytes: usize) -> bool {
        let share = JOURNAL_SHARES.writer_records as u64;
        let line = BURST as u64 + (share - BURST as u64) * elapsed.min(WEEK) / WEEK;
        let byte_line = JOURNAL_SHARES.writer_bytes as u64 * line / share;
        (used.writer_records as u64) < line
            && (used.writer_bytes as u64).saturating_add(bytes as u64) <= byte_line
    }

    /// The first second of a week (counted from its start) at which [`Self::under_pace`] admits
    /// one more record of `bytes` beside `used`, or `None` when nothing does within the week.
    pub(crate) fn paced_at(used: &Used, bytes: usize) -> Option<u64> {
        if !Self::under_pace(used, WEEK, bytes) {
            return None;
        }
        // The line only grows with the week, so the first admitting second is a bisection.
        let (mut low, mut high) = (0, WEEK);
        while low < high {
            let mid = low + (high - low) / 2;
            if Self::under_pace(used, mid, bytes) {
                high = mid;
            } else {
                low = mid + 1;
            }
        }
        Some(low)
    }

    /// Whether a flush is due at `now` in week `period` (design §3.9): the week's first record
    /// (its `sum`, even idle); otherwise, while under the pace line, a `sum` that must say the
    /// journal was turned off or on, and with the journal on a sharp event, 128 waiting events,
    /// or six hours since the last flush with events or rows waiting. With the journal off
    /// (`off`, `[alab] journal = false`) nothing but the `sum` is ever due.
    pub(crate) fn due(
        &self,
        outbox: &[Event],
        owed: bool,
        now: u64,
        period: u64,
        under: bool,
        off: bool,
    ) -> bool {
        if self.sum_period != Some(period) {
            return true;
        }
        if !under {
            return false;
        }
        if self.sum_off != off {
            return true;
        }
        if off {
            return false;
        }
        outbox.iter().any(|e| e.kind.sharp())
            || outbox.len() >= BATCH_EVENTS
            || ((owed || !outbox.is_empty()) && now >= self.flushed_at.saturating_add(ROUTINE_S))
    }

    /// The `use` rows due now: a row that changed and was last written an hour or more ago, and
    /// once a week the row of every account used in the last 180 days (the weekly carry).
    pub(crate) fn uses_due(&self, n: u64, period: u64, now: u64) -> Result<Vec<(Pk, Value)>> {
        let mut due = Vec::new();
        for (pk, facts) in &self.accounts {
            let row = facts.use_row(n, period, now, caps::USE_ROW)?;
            let said = facts.stated.as_ref();
            let changed = said.is_none_or(|s| s.value != content(&row))
                && said.is_none_or(|s| now >= s.at.saturating_add(USE_EVERY));
            let carry = said.is_some_and(|s| s.period < period)
                && facts.used.saturating_add(CARRY_S) >= now;
            if changed || carry {
                due.push((*pk, row));
            }
        }
        Ok(due)
    }

    /// The `relay` rows due now: a duty bucket's row that changed and was last written six
    /// hours or more ago, and once a week each duty bucket's row (the weekly carry).
    pub(crate) fn relays_due(
        &self,
        heard: &Heard,
        me: &[u8; 16],
        n: u64,
        period: u64,
        now: u64,
    ) -> Result<Vec<(u8, Value)>> {
        let mut due = Vec::new();
        for bucket in heard.duty(me) {
            let Some(row) = heard.row(bucket, n, period, caps::RELAY_ROW)? else {
                continue;
            };
            let said = self.relays.get(&bucket);
            let changed = said.is_none_or(|s| s.value != content(&row))
                && said.is_none_or(|s| now >= s.at.saturating_add(ROUTINE_S));
            let carry = said.is_some_and(|s| s.period < period);
            if changed || carry {
                due.push((bucket, row));
            }
        }
        Ok(due)
    }

    /// The `sum` row (design §4.17): `journal` says `off` when `off`, `local` while the last
    /// flush was kept, else `on`; `used` counts accounts, a sub-account's row its account's.
    pub(crate) fn sum_row(
        &self,
        presence: &Presence,
        n: u64,
        period: u64,
        now: u64,
        off: bool,
    ) -> Value {
        let start = period.saturating_mul(WEEK);
        let used = self
            .accounts
            .iter()
            .filter(|(_, f)| f.used >= start)
            .map(|(pk, f)| f.account.unwrap_or(*pk))
            .collect::<BTreeSet<Pk>>()
            .len();
        let mut row = Map::new();
        row.insert("r".into(), Value::from(VERSION));
        row.insert("n".into(), Value::from(n));
        row.insert("period".into(), Value::from(period));
        row.insert("at".into(), Value::from(now));
        if let Some(label) = &presence.label {
            row.insert("label".into(), Value::from(label.as_str()));
        }
        row.insert("aterm".into(), Value::from(env!("CARGO_PKG_VERSION")));
        if let Some(os) = presence.os {
            row.insert("os".into(), Value::from(os));
        }
        row.insert("clis".into(), Value::Array(Vec::new()));
        row.insert("used".into(), Value::from(used as u64));
        row.insert(
            "journal".into(),
            Value::from(if off {
                "off"
            } else if self.kept.is_some() {
                "local"
            } else {
                "on"
            }),
        );
        Value::Object(row)
    }

    /// One flush record from `outbox`, the rows due and `heard`'s relays, at counter `n` (design
    /// §4.17 "Record packing"): the `ev` batch first, then `sum`, then the changed `use` rows,
    /// then the `relay` rows, while the record stays within 64 changes and 60 KiB; what does
    /// not fit goes in the next flush. With the journal off (`off`) the record holds the `sum`
    /// row alone, saying so: no event, `use` or `relay` row is published.
    ///
    /// # Errors
    /// A row cannot be built within its cap.
    #[expect(
        clippy::too_many_arguments,
        reason = "one record's inputs, each distinct"
    )]
    pub(crate) fn plan(
        &self,
        outbox: &[Event],
        heard: &Heard,
        presence: &Presence,
        me: &[u8; 16],
        n: u64,
        period: u64,
        now: u64,
        off: bool,
    ) -> Result<Planned> {
        let uuid = aterm_codec::hex::encode_lower(me);
        let mut planned = Planned {
            off,
            ..Planned::default()
        };
        let outbox = if off { &[] } else { outbox };
        let values: Vec<Value> = outbox.iter().map(Event::to_value).collect();
        let batch = |kept: &[Value]| json!({"r": VERSION, "n": n, "e": kept});
        let events = fit(&values, caps::EV_BATCH, batch)?;
        if events > 0 {
            planned.changes.push(Change {
                collection: super::EV.name.into(),
                key: format!("{uuid}.{n}"),
                value: Some(batch(&values[..events])),
            });
            planned.events = events;
        }
        let offer = |planned: &mut Planned, change: Change| {
            planned.changes.push(change);
            let fit = fits(&planned.changes);
            if !fit {
                planned.changes.pop();
            }
            fit
        };
        if self.sum_period != Some(period) || self.sum_off != off {
            let row = self.sum_row(presence, n, period, now, off);
            let change = Change {
                collection: super::SUM.name.into(),
                key: uuid.clone(),
                value: Some(row),
            };
            planned.sum = offer(&mut planned, change);
        }
        if off {
            return Ok(planned);
        }
        for (pk, row) in self.uses_due(n, period, now)? {
            let change = Change {
                collection: super::USE.name.into(),
                key: format!("{}.{uuid}", pk.as_str()),
                value: Some(row.clone()),
            };
            if offer(&mut planned, change) {
                planned.uses.push((pk, row));
            }
        }
        for (bucket, row) in self.relays_due(heard, me, n, period, now)? {
            let change = Change {
                collection: super::RELAY.name.into(),
                key: format!("rl-{bucket:x}.{uuid}"),
                value: Some(row.clone()),
            };
            if offer(&mut planned, change) {
                planned.relays.push((bucket, row));
            }
        }
        if !fits(&planned.changes) {
            return Err(Error::new("a journal flush record passes its limits"));
        }
        Ok(planned)
    }

    /// Record that `planned` went out at counter `n` in week `period`.
    pub(crate) fn wrote(&mut self, planned: &Planned, n: u64, period: u64, now: u64) {
        self.n = self.n.max(n);
        self.flushed_at = now;
        self.kept = None;
        if planned.sum {
            self.sum_period = Some(period);
            self.sum_off = planned.off;
        }
        for (pk, row) in &planned.uses {
            if let Some(facts) = self.accounts.get_mut(pk) {
                facts.stated = Some(Stated {
                    value: content(row),
                    at: now,
                    period,
                });
            }
        }
        for (bucket, row) in &planned.relays {
            self.relays.insert(
                *bucket,
                Stated {
                    value: content(row),
                    at: now,
                    period,
                },
            );
        }
    }

    /// The book as its file holds it.
    pub(crate) fn to_value(&self) -> Value {
        let stated = |s: &Stated| json!({"value": s.value, "at": s.at, "period": s.period});
        let accounts: Map = self
            .accounts
            .iter()
            .map(|(pk, f)| {
                let mut o = Map::new();
                o.insert("first".into(), Value::from(f.first));
                o.insert("last_start".into(), Value::from(f.last_start));
                o.insert("last_end".into(), Value::from(f.last_end));
                o.insert("starts".into(), json!(f.starts));
                o.insert("used".into(), Value::from(f.used));
                if let Some(s) = &f.signin {
                    o.insert(
                        "signin".into(),
                        json!({"state": s.state, "t": s.t, "cli": s.cli}),
                    );
                }
                o.insert(
                    "quota".into(),
                    Value::Array(f.quota.values().map(Event::to_value).collect()),
                );
                o.insert(
                    "walls".into(),
                    Value::Array(
                        f.walls
                            .iter()
                            .map(|(w, (u, t))| json!({"w": w.word(), "until": u, "t": t}))
                            .collect(),
                    ),
                );
                if let Some((word, t)) = &f.plan {
                    o.insert("plan".into(), json!({"word": word, "t": t}));
                }
                if let Some(key) = &f.key {
                    o.insert("key".into(), Value::from(key.as_str()));
                }
                if let Some(account) = &f.account {
                    o.insert("account".into(), Value::from(account.as_str()));
                }
                if let Some(s) = &f.stated {
                    o.insert("stated".into(), stated(s));
                }
                (pk.as_str().to_owned(), Value::Object(o))
            })
            .collect();
        json!({
            "v": VERSION,
            "n": self.n,
            "sum_period": self.sum_period,
            "sum_off": self.sum_off,
            "took_off": self.took_off,
            "flushed_at": self.flushed_at,
            "checked_at": self.checked_at,
            "held_until": self.held_until,
            "kept": self.kept,
            "accounts": accounts,
            "relays": self.relays.iter()
                .map(|(b, s)| (b.to_string(), stated(s)))
                .collect::<Map>(),
            "heard": self.heard.iter()
                .map(|(m, ns)| (m.clone(), json!(ns.iter().copied().collect::<Vec<u64>>())))
                .collect::<Map>(),
            "known": self.known.iter()
                .map(|((a, k), e)| json!({"a": a.as_str(), "k": k.word(), "m": aterm_codec::hex::encode_lower(&e.m),
                    "p": e.p, "t": e.t}))
                .collect::<Vec<Value>>(),
        })
    }

    /// The book a file holds; anything unreadable in it is left out, never an error, since the
    /// book is this Mac's own cache of what it saw and said.
    pub(crate) fn from_value(v: &Value) -> Self {
        let u = |v: Option<&Value>| v.and_then(Value::as_u64).unwrap_or(0);
        let stated = |v: Option<&Value>| {
            let v = v?;
            Some(Stated {
                value: v.get("value")?.clone(),
                at: v.get("at")?.as_u64()?,
                period: v.get("period")?.as_u64()?,
            })
        };
        let mut book = Self {
            n: u(v.get("n")),
            sum_period: v.get("sum_period").and_then(Value::as_u64),
            sum_off: v.get("sum_off").and_then(Value::as_bool).unwrap_or(false),
            took_off: v.get("took_off").and_then(Value::as_bool).unwrap_or(false),
            flushed_at: u(v.get("flushed_at")),
            checked_at: u(v.get("checked_at")),
            held_until: u(v.get("held_until")),
            kept: v.get("kept").and_then(Value::as_str).map(str::to_owned),
            ..Self::default()
        };
        for (pk, f) in v
            .get("accounts")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
        {
            let Some(pk) = Pk::new(pk) else { continue };
            let mut facts = Facts {
                first: u(f.get("first")),
                last_start: u(f.get("last_start")),
                last_end: u(f.get("last_end")),
                used: u(f.get("used")),
                starts: f
                    .get("starts")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_u64)
                    .take(MAX_STARTS)
                    .collect(),
                key: f.get("key").and_then(Value::as_str).map(str::to_owned),
                account: f.get("account").and_then(Value::as_str).and_then(Pk::new),
                stated: stated(f.get("stated")),
                ..Facts::default()
            };
            facts.signin = f.get("signin").and_then(|s| {
                let word = s.get("state")?.as_str()?;
                Some(SignIn {
                    state: SIGNIN_STATES.into_iter().find(|w| *w == word)?,
                    t: s.get("t")?.as_u64()?,
                    cli: s.get("cli")?.as_str()?.to_owned(),
                })
            });
            for e in f
                .get("quota")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Event::from_value)
            {
                if let Some(w) = e.window {
                    facts.quota.insert(w, e);
                }
            }
            for w in f
                .get("walls")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                if let (Some(window), Some(t)) = (
                    w.get("w").and_then(Value::as_str).and_then(Window::parse),
                    w.get("t").and_then(Value::as_u64),
                ) {
                    facts
                        .walls
                        .insert(window, (w.get("until").and_then(Value::as_u64), t));
                }
            }
            facts.plan = f
                .get("plan")
                .and_then(|p| Some((p.get("word")?.as_str()?.to_owned(), p.get("t")?.as_u64()?)));
            book.accounts.insert(pk, facts);
        }
        for (b, s) in v
            .get("relays")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
        {
            if let (Ok(b), Some(s)) = (b.parse::<u8>(), stated(Some(s)))
                && b < caps::RELAY_BUCKETS
                && s.value.get("b").and_then(Value::as_u64) == Some(u64::from(b))
            {
                book.relays.insert(b, s);
            }
        }
        for e in v
            .get("known")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let parsed = (|| {
                let account = Pk::new(e.get("a")?.as_str()?)?;
                let kind = Rk::parse(e.get("k")?.as_str()?)?;
                let m = crate::database::decode_hex::<16>(e.get("m")?.as_str()?).ok()?;
                let entry = Entry {
                    m,
                    t: e.get("t")?.as_u64()?,
                    p: e.get("p")?.as_u64()?,
                };
                Some(((account, kind), entry))
            })();
            if let Some((key, entry)) = parsed {
                book.known.insert(key, entry);
            }
        }
        for (m, ns) in v
            .get("heard")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
        {
            book.heard.insert(
                m.clone(),
                ns.as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_u64)
                    .collect(),
            );
        }
        book
    }

    /// The book in `spool`'s folder, or an empty one.
    pub(crate) fn load(spool: &super::Spool) -> Self {
        spool
            .read_bytes(BOOK, MAX_BOOK)
            .ok()
            .and_then(|bytes| aterm_json::from_slice::<Value>(&bytes).ok())
            .map(|v| Self::from_value(&v))
            .unwrap_or_default()
    }

    /// Save the book in `spool`'s folder.
    ///
    /// # Errors
    /// The book cannot be encoded or written.
    pub(crate) fn save(&self, spool: &super::Spool) -> Result<()> {
        let bytes = aterm_json::to_vec(&self.to_value())
            .map_err(|_| Error::new("the journal book could not be encoded"))?;
        spool.write(BOOK, &bytes)?;
        Ok(())
    }
}
