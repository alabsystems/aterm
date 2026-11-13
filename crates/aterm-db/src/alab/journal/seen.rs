// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What the journal window says of each account (design §7, §8, step 11): who last used it and
//! on which Mac, where it is signed in, its current quota per window, and its log. Every answer
//! is derived from the records every Mac holds (`ev` batches, `use` rows, `relay` rows, `place`
//! rows and each Mac's latest `sum` row), so every Mac computes the same answer from the same merged records; nothing is written
//! and no provider is asked. A Mac is named here by the Commons machine uuid that wrote the
//! record; [`super::super::view::LabView::bound`] names its machine.
//!
//! - **Last use** is the newest `use` fact by (time, machine) over session events, every Mac's
//!   `use` row and every relay entry, each believed only within a week of its source table
//!   ([`super::relay::believed`]), so a relay carries it past the weeks that held it, and the
//!   `since` of every live claims-ring claim ([`Seen::claim`]).
//! - **Signed in** is, per (account, Mac), the latest sign-in word of that Mac's `use` row and its
//!   in-window sign-in events ([`Event::signin`]): `in`, `out`, `ended`, `wrong` or `failed`, and
//!   none once the Mac withdrew its word.
//! - **Quota** groups a window's readings by their reset (the window's identity: one window's
//!   readings name one reset, within each reading's resolution), drops a group a later group's
//!   reset puts a whole window earlier, takes the most used of the overlapping groups left and of
//!   the reset-less readings made inside the window (readings are snapshots and never add up, and
//!   a lower reading frees nothing), shows the reset as unknown where those groups disagree beyond
//!   their resolution, and says "reset at T, not read since" once every reset read has passed with
//!   nothing read since, never a guessed 0. A
//!   workspace's readings are its sub-account's (`sa-` key) and are folded under the account.

use super::{
    EV, PLACE, RELAY, SUM, USE,
    event::{Event, Kind, Pk, Res, SIGNIN_UNKNOWN, SIGNIN_WORDS, Window},
    place::{self, Places},
    relay::{Heard, Rk},
};
use crate::typed::{keys, owned};
use astream_wire::sync::{Actor, Digest};
use aterm_codec::hex;
use aterm_json::Value;
use std::collections::BTreeMap;

/// One Mac's latest sign-in word for one account.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignedIn {
    /// `in`, `out`, `ended`, `wrong` or `failed`.
    pub state: &'static str,
    pub t: u64,
    pub cli: String,
}

/// The sign-in word a `use` row's `signin.state` holds, as a static word.
fn state_word(word: &str) -> Option<&'static str> {
    SIGNIN_WORDS.into_iter().find(|state| *state == word)
}

/// One quota reading of one window, by the Mac that made it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reading {
    /// The Commons machine uuid (32 hex) of the Mac that read it.
    pub uuid: String,
    /// When it was read.
    pub t: u64,
    pub pct_x10: u16,
    pub reset: Option<u64>,
    pub res: Option<Res>,
    /// Where it came from (`statusline`, `rollout`, …).
    pub src: String,
}

/// A standing wall one Mac saw.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Wall {
    pub uuid: String,
    pub t: u64,
    pub until: Option<u64>,
}

/// One window's quota, as §7 reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Usage {
    /// The most used of the current window's readings. `reset` is `None` where the current
    /// readings disagree on the reset beyond their resolution (or none named one).
    Current {
        pct_x10: u16,
        reset: Option<u64>,
        /// The time, Mac and source of the reading that gave the figure.
        read: u64,
        by: String,
        src: String,
    },
    /// Every reset read has passed and nothing was read since: the reset, and when it was read.
    ResetUnread { reset: u64, read: u64, by: String },
}

/// One window of one account or sub-account.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowQuota {
    /// The account (`ac-`) or the workspace's sub-account (`sa-`).
    pub pk: String,
    pub window: Window,
    /// `None` where only a wall is known.
    pub usage: Option<Usage>,
    /// A wall still standing at the read's `now`.
    pub wall: Option<Wall>,
}

/// One event of the log, with the Mac that logged it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Logged {
    pub uuid: String,
    /// Its batch's counter.
    pub n: u64,
    pub event: Event,
    /// Read from this Mac's heard archive rather than a window table.
    pub heard: bool,
}

/// What the journal window says ([`Seen::of`]).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Seen {
    /// Each account's newest use: the Mac (32 hex) and when, by that Mac's clock.
    pub last_use: BTreeMap<String, (String, u64)>,
    /// Each account's sign-in word per Mac (32 hex).
    pub signins: BTreeMap<String, BTreeMap<String, SignedIn>>,
    readings: BTreeMap<String, BTreeMap<Window, Vec<Reading>>>,
    walls: BTreeMap<String, BTreeMap<Window, Wall>>,
    /// Each sub-account key to the account it belongs to.
    pub sub_of: BTreeMap<String, String>,
    /// Every in-window event, by (time, Mac, counter).
    pub events: Vec<Logged>,
    /// The live `place` rows.
    pub places: Places,
    /// Each Mac's latest `sum` row (32 hex uuid to the row): its label, version, OS, the count
    /// of accounts it used and its `journal` word, as that Mac wrote it.
    pub sums: BTreeMap<String, Value>,
    /// Each account's live claims-ring claims ([`Self::claim`]): which Macs hold it now.
    pub claims: BTreeMap<String, Vec<Claimed>>,
    /// When the read was made, Unix seconds.
    pub now: u64,
}

/// One live claim of the claims ring (F19) on an account: the Mac (32 hex) holds it from `since`
/// until `until` unless it writes its row again, as `home` (its home Mac) or `lease`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Claimed {
    pub uuid: String,
    pub kind: String,
    pub since: u64,
    pub until: u64,
}

/// How far apart two readings' resets may be and still name one window: each reading's
/// resolution (§7: exact for status-line and API resets, an hour for "Resets 1pm", a day for
/// "Resets Sep 23", the clock-skew bound for a local `now + reset_after`). An exact reset still
/// gets a minute, since two Macs round one server time to the second differently.
const fn tolerance(res: Option<Res>) -> u64 {
    match res {
        Some(Res::Exact | Res::Minute) => 60,
        Some(Res::Hour) => 3_600,
        Some(Res::Day) => 86_400,
        // The clock-skew bound: the claims ring reads a Mac's clock as split past 15 minutes.
        Some(Res::Local) | None => 900,
    }
}

impl Seen {
    /// Nothing seen, as of `now`: a Mac that holds no Commons it is a member of.
    #[must_use]
    pub fn empty(now: u64) -> Self {
        Self {
            now,
            ..Self::default()
        }
    }

    /// Read the window's typed operations `ops` at `now`, each row's writer the machine `resolve`
    /// names for its record.
    pub fn of(
        ops: &[owned::Op],
        resolve: &dyn Fn(&Digest, &Actor) -> Option<[u8; 16]>,
        now: u64,
    ) -> Self {
        let mut seen = Self {
            now,
            ..Self::default()
        };
        let mut heard = Heard::default();
        let ev = owned::read_log(&EV, ops, resolve);
        for entry in &ev.entries {
            let uuid = hex::encode_lower(&entry.machine);
            for event in entry
                .value
                .get("e")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Event::from_value)
            {
                heard.read_event(entry.machine, entry.period, &event);
                seen.fold_event(&uuid, &event);
                seen.events.push(Logged {
                    uuid: uuid.clone(),
                    n: entry.n,
                    event,
                    heard: false,
                });
            }
        }
        let uses = owned::read_owned(&USE, ops, resolve);
        for (key, row) in &uses.rows {
            let Some((pk, _)) = key.rsplit_once('.') else {
                continue;
            };
            let Some(pk) = Pk::new(pk) else { continue };
            let account = seen.row_account(pk, &row.value);
            let uuid = hex::encode_lower(&row.machine);
            if let Some(account) = account {
                heard.read_use(account, row.machine, row.rank.0, &row.value);
                seen.sign_use(account.as_str(), &uuid, &row.value);
            }
            seen.fold_use(pk.as_str(), &uuid, &row.value);
        }
        let relays = owned::read_owned(&RELAY, ops, resolve);
        for (key, row) in &relays.rows {
            if super::flush::key_relay_bucket(key)
                .is_some_and(|b| row.value.get("b").and_then(Value::as_u64) == Some(u64::from(b)))
            {
                // No Mac reads as this one: coverage is not asked here, only the facts.
                heard.read_relay(row.machine, &[0xff; 16], &row.value, row.rank.0);
            }
        }
        for ((account, kind), entry) in &heard.newest {
            if *kind == Rk::Use && account.as_str().starts_with("ac-") {
                seen.last_use.insert(
                    account.as_str().to_owned(),
                    (hex::encode_lower(&entry.m), entry.t),
                );
            }
        }
        seen.places = place::live(&owned::read_owned(&PLACE, ops, resolve), now);
        seen.sums = owned::read_owned(&SUM, ops, resolve)
            .rows
            .into_values()
            .map(|row| (hex::encode_lower(&row.machine), row.value))
            .collect();
        seen.drop_withdrawn();
        seen.sort_events();
        seen
    }

    /// The sign-in word a `use` row of `uuid`'s states of `account` (`signin {state, t, cli}`),
    /// folded as an event's is: the row is that Mac's own fold, so it stands over an event of
    /// the same second.
    fn sign_use(&mut self, account: &str, uuid: &str, row: &Value) {
        let Some(signin) = row.get("signin") else {
            return;
        };
        let state = signin
            .get("state")
            .and_then(Value::as_str)
            .and_then(state_word);
        let t = signin.get("t").and_then(Value::as_u64).unwrap_or(0);
        let cli = signin.get("cli").and_then(Value::as_str).unwrap_or("");
        if let Some(state) = state {
            self.sign(account, uuid, state, t, cli);
        }
    }

    /// A withdrawn word stood only so that no older event or row restored it: no reader is
    /// given it, and a Mac with no word left says nothing of the account.
    fn drop_withdrawn(&mut self) {
        self.signins.retain(|_, by_mac| {
            by_mac.retain(|_, held| held.state != SIGNIN_UNKNOWN);
            !by_mac.is_empty()
        });
    }

    /// Test seam: what a peer reads of the sign-in words of the Mac `uuid` from what that Mac
    /// publishes, its `events` in the order an `ev` batch holds them and then its `use` rows
    /// (each an account's key and its row), folded by the code [`Self::of`] folds a window with.
    #[cfg(any(test, debug_assertions))]
    pub(super) fn of_published(
        uuid: &str,
        events: &[Event],
        uses: &[(Pk, Value)],
        now: u64,
    ) -> Self {
        let mut seen = Self::empty(now);
        for event in events {
            seen.fold_event(uuid, event);
        }
        for (account, row) in uses {
            seen.sign_use(account.as_str(), uuid, row);
        }
        seen.drop_withdrawn();
        seen
    }

    /// Add the batches of this Mac's heard archive from `since` on (§7: the log reaches past the
    /// ring's window, labelled "heard by this Mac"): every event no window batch already holds.
    pub(crate) fn hear(&mut self, batches: &[super::heard::Batch]) {
        if batches.is_empty() {
            return;
        }
        let mut added = Vec::new();
        {
            let held: std::collections::BTreeSet<(&str, u64)> = self
                .events
                .iter()
                .map(|logged| (logged.uuid.as_str(), logged.n))
                .collect();
            for (uuid, n, _, events) in batches {
                if held.contains(&(uuid.as_str(), *n)) {
                    continue;
                }
                for event in events {
                    added.push(Logged {
                        uuid: uuid.clone(),
                        n: *n,
                        event: *event,
                        heard: true,
                    });
                }
            }
        }
        if added.is_empty() {
            return;
        }
        self.events.extend(added);
        self.sort_events();
    }

    fn sort_events(&mut self) {
        self.events.sort_by(|a, b| {
            (a.event.t, &a.uuid, a.n, a.event).cmp(&(b.event.t, &b.uuid, b.n, b.event))
        });
        self.events
            .dedup_by(|a, b| a.uuid == b.uuid && a.n == b.n && a.event == b.event);
    }

    /// The account a `use` row at `pk` is about: itself, or a sub-account's `account` column
    /// where the key derives from it and the row's `key` (a sub-account key is a one-way digest
    /// of the two, so a row naming another account is no row of it).
    fn row_account(&mut self, pk: Pk, row: &Value) -> Option<Pk> {
        if pk.as_str().starts_with("ac-") {
            return Some(pk);
        }
        let account = row.get("account").and_then(Value::as_str)?;
        let key = row.get("key").and_then(Value::as_str)?;
        (keys::subaccount_pk(account, key).ok()? == pk.as_str()).then_some(())?;
        if !self.sub_of.contains_key(pk.as_str()) {
            self.sub_of
                .insert(pk.as_str().to_owned(), account.to_owned());
        }
        Pk::new(account)
    }

    fn sign(&mut self, account: &str, uuid: &str, state: &'static str, t: u64, cli: &str) {
        if let Some(by_mac) = self.signins.get_mut(account) {
            if let Some(held) = by_mac.get_mut(uuid) {
                if held.t <= t {
                    held.state = state;
                    held.t = t;
                    held.cli.clear();
                    held.cli.push_str(cli);
                }
                return;
            }
            by_mac.insert(
                uuid.to_owned(),
                SignedIn {
                    state,
                    t,
                    cli: cli.to_owned(),
                },
            );
            return;
        }
        let mut by_mac = BTreeMap::new();
        by_mac.insert(
            uuid.to_owned(),
            SignedIn {
                state,
                t,
                cli: cli.to_owned(),
            },
        );
        self.signins.insert(account.to_owned(), by_mac);
    }

    fn read(&mut self, pk: &str, window: Window, reading: Reading) {
        if let Some(by_window) = self.readings.get_mut(pk) {
            let list = by_window.entry(window).or_default();
            if !list.contains(&reading) {
                list.push(reading);
            }
            return;
        }
        let mut by_window = BTreeMap::new();
        by_window.insert(window, vec![reading]);
        self.readings.insert(pk.to_owned(), by_window);
    }

    fn wall(&mut self, pk: &str, window: Window, wall: Wall) {
        if let Some(by_window) = self.walls.get_mut(pk) {
            if by_window.get(&window).is_none_or(|held| held.t <= wall.t) {
                by_window.insert(window, wall);
            }
            return;
        }
        let mut by_window = BTreeMap::new();
        by_window.insert(window, wall);
        self.walls.insert(pk.to_owned(), by_window);
    }

    fn fold_event(&mut self, uuid: &str, event: &Event) {
        let Some(account) = event.account else {
            return;
        };
        if let Some(state) = event.signin() {
            self.sign(account.as_str(), uuid, state, event.t, event.cli.as_str());
        }
        // A workspace's figures are its sub-account's (§4.13's derivation, no lookup).
        let sub_pk = match &event.sub {
            Some(sub) => match keys::subaccount_pk(account.as_str(), sub.as_str()) {
                Ok(pk) => {
                    if !self.sub_of.contains_key(&pk) {
                        self.sub_of.insert(pk.clone(), account.as_str().to_owned());
                    }
                    Some(pk)
                }
                Err(_) => return,
            },
            None => None,
        };
        let Some(window) = event.window else {
            return;
        };
        let pk = sub_pk.as_deref().unwrap_or(account.as_str());
        match event.kind {
            Kind::Quota => {
                if let Some(pct_x10) = event.pct_x10 {
                    self.read(
                        pk,
                        window,
                        Reading {
                            uuid: uuid.to_owned(),
                            t: event.t,
                            pct_x10,
                            reset: event.reset,
                            res: event.res,
                            src: event.src.word().to_owned(),
                        },
                    );
                }
            }
            Kind::Wall => self.wall(
                pk,
                window,
                Wall {
                    uuid: uuid.to_owned(),
                    t: event.t,
                    until: event.until,
                },
            ),
            Kind::WallCleared => {
                if let Some(by_window) = self.walls.get_mut(pk)
                    && by_window.get(&window).is_some_and(|held| held.t <= event.t)
                {
                    by_window.remove(&window);
                    if by_window.is_empty() {
                        self.walls.remove(pk);
                    }
                }
            }
            _ => {}
        }
    }

    /// A `use` row's quota and wall lists (design §4.17: `quota: list<{w, p, r, rr, t, src}>`,
    /// `wall: list<{w, until, t}>`), at the row's own key.
    fn fold_use(&mut self, pk: &str, uuid: &str, row: &Value) {
        for q in row
            .get("quota")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let parsed = (|| {
                let window = Window::parse(q.get("w")?.as_str()?)?;
                let pct_x10 = u16::try_from(q.get("p")?.as_u64()?)
                    .ok()
                    .filter(|p| *p <= 1000)?;
                Some((
                    window,
                    Reading {
                        uuid: uuid.to_owned(),
                        t: q.get("t")?.as_u64()?,
                        pct_x10,
                        reset: q.get("r").and_then(Value::as_u64),
                        res: q.get("rr").and_then(Value::as_str).and_then(Res::parse),
                        src: q
                            .get("src")
                            .and_then(Value::as_str)
                            .filter(|s| super::event::Source::parse(s).is_some())
                            .unwrap_or("relay")
                            .to_owned(),
                    },
                ))
            })();
            if let Some((window, reading)) = parsed {
                self.read(pk, window, reading);
            }
        }
        for w in row
            .get("wall")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let parsed = (|| {
                Some((
                    Window::parse(w.get("w")?.as_str()?)?,
                    Wall {
                        uuid: uuid.to_owned(),
                        t: w.get("t")?.as_u64()?,
                        until: w.get("until").and_then(Value::as_u64),
                    },
                ))
            })();
            if let Some((window, wall)) = parsed {
                self.wall(pk, window, wall);
            }
        }
    }

    /// The quota of account `account` per window, its sub-accounts' windows after its own (§7:
    /// folded per sub-account), each read as of [`Self::now`].
    pub fn quota(&self, account: &str) -> Vec<WindowQuota> {
        let mut pks: Vec<&str> = vec![account];
        pks.extend(
            self.sub_of
                .iter()
                .filter(|(_, of)| of.as_str() == account)
                .map(|(sa, _)| sa.as_str()),
        );
        let mut out = Vec::new();
        for pk in pks {
            let readings = self.readings.get(pk);
            let walls = self.walls.get(pk);
            if readings.is_none() && walls.is_none() {
                continue;
            }
            for &window in Window::ALL {
                let usage = readings
                    .and_then(|by_window| by_window.get(&window))
                    .and_then(|readings| usage(window, readings, self.now));
                let wall = walls
                    .and_then(|by_window| by_window.get(&window))
                    .filter(|wall| wall.until.is_none_or(|until| until > self.now))
                    .cloned();
                if usage.is_some() || wall.is_some() {
                    out.push(WindowQuota {
                        pk: pk.to_owned(),
                        window,
                        usage,
                        wall,
                    });
                }
            }
        }
        out
    }

    /// Fold the claims ring's rows (`accounts::claims::Read::rows`: each Mac's latest row, by the
    /// machine that signed it) into what is seen (§7 "Who last used account A": live claims
    /// naming A count): each live claim (its row's `until` still ahead of [`Self::now`]) is listed
    /// under its account, keyed as the claims ring keys it ([`keys::account_pk`] of its pool and
    /// email), and its `since`, clamped to `now`, is a use of that account by that Mac, so last
    /// use is never older than a claim held now. A claim this build cannot read is left out, the
    /// rest of its row still counts; a lapsed row says nothing.
    pub fn claim(&mut self, rows: &[([u8; 16], Value)]) {
        for (machine, body) in rows {
            let until = body
                .get("until")
                .and_then(Value::as_i64)
                .and_then(|t| u64::try_from(t).ok());
            let Some(until) = until.filter(|until| *until > self.now) else {
                continue;
            };
            let uuid = hex::encode_lower(machine);
            for claim in body
                .get("claims")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let field = |name: &str| claim.get(name).and_then(Value::as_str);
                let (Some(pool), Some(email), Some(kind)) =
                    (field("pool"), field("email"), field("kind"))
                else {
                    continue;
                };
                let since = claim
                    .get("since")
                    .and_then(Value::as_i64)
                    .and_then(|t| u64::try_from(t).ok());
                let (Ok(pk), Some(since)) = (keys::account_pk(pool, email), since) else {
                    continue;
                };
                let since = since.min(self.now);
                if self.last_use.get(&pk).is_none_or(|(_, t)| *t < since) {
                    self.last_use.insert(pk.clone(), (uuid.clone(), since));
                }
                self.claims.entry(pk).or_default().push(Claimed {
                    uuid: uuid.clone(),
                    kind: kind.to_owned(),
                    since,
                    until,
                });
            }
        }
    }

    /// Every account the window says anything of, sorted.
    pub fn accounts(&self) -> std::collections::BTreeSet<String> {
        let mut out: std::collections::BTreeSet<String> = self.last_use.keys().cloned().collect();
        out.extend(self.signins.keys().cloned());
        for pk in self.readings.keys().chain(self.walls.keys()) {
            out.insert(self.sub_of.get(pk).unwrap_or(pk).clone());
        }
        out
    }
}

/// The shortest span between one reset of `window` and the next: a group of readings whose reset
/// lies that far (less its tolerance) before another group's names an earlier window, not a
/// disagreement about this one. `None` for a window with no fixed length (credits), where only a
/// passed reset marks an earlier window.
const fn window_length(window: Window) -> Option<u64> {
    match window {
        Window::FiveHour => Some(5 * 3_600),
        Window::Week | Window::WeekModel => Some(7 * 86_400),
        Window::Day => Some(86_400),
        // The shortest month.
        Window::Month => Some(28 * 86_400),
        Window::Credits => None,
    }
}

/// The reading a figure is credited to: the most used, the newest among equals.
fn figure<'r>(list: impl IntoIterator<Item = &'r Reading>) -> Option<&'r Reading> {
    list.into_iter()
        .max_by(|a, b| (a.pct_x10, a.t, &a.uuid).cmp(&(b.pct_x10, b.t, &b.uuid)))
}

/// One window's usage from its readings at `now` (§7).
///
/// Readings that name a reset are grouped by it (window identity, each within its resolution's
/// tolerance). A group a later group's reset puts a whole window before it is an earlier window
/// and is dropped, even while its own reset is inside its tolerance; the groups left overlap,
/// and the figure is the most used reading across them and across the reset-less readings made
/// inside the window, credited to that reading. More than one group left gives an unknown reset.
/// Once every reset has passed, a reset-less reading made since is the figure (its reset
/// unknown); with none, the window says "reset at T, not read since".
fn usage(window: Window, readings: &[Reading], now: u64) -> Option<Usage> {
    let current_of = |r: &Reading| Usage::Current {
        pct_x10: r.pct_x10,
        reset: None,
        read: r.t,
        by: r.uuid.clone(),
        src: r.src.clone(),
    };
    let (reset_known, reset_less): (Vec<&Reading>, Vec<&Reading>) =
        readings.iter().partition(|r| r.reset.is_some());
    let reset_of = |r: &Reading| r.reset.unwrap_or(0);
    let Some(last_reset) = reset_known.iter().map(|r| reset_of(r)).max() else {
        // No reading names its window: the newest one, its reset unknown.
        return reset_less
            .iter()
            .max_by_key(|r| (r.t, r.uuid.as_str()))
            .map(|r| current_of(r));
    };
    let mut current: Vec<&Reading> = reset_known
        .iter()
        .copied()
        .filter(|r| reset_of(r).saturating_add(tolerance(r.res)) > now)
        .collect();
    if current.is_empty() {
        // Every reset has passed: only a reading made since speaks for the new window.
        if let Some(since) = figure(reset_less.iter().copied().filter(|r| r.t > last_reset)) {
            return Some(current_of(since));
        }
        let last = reset_known.iter().max_by_key(|r| (r.t, r.uuid.as_str()))?;
        return Some(Usage::ResetUnread {
            reset: last_reset,
            read: last.t,
            by: last.uuid.clone(),
        });
    }
    // Group the current readings by window identity: sorted by reset, a reading joins the group
    // of the one before it when the two resets lie within the wider of their resolutions.
    current.sort_by_key(|r| (r.reset, r.t));
    let mut groups: Vec<Vec<&Reading>> = Vec::new();
    for r in current {
        match groups.last_mut() {
            Some(group)
                if group.last().is_some_and(|prev| {
                    reset_of(r).abs_diff(reset_of(prev))
                        <= tolerance(prev.res).max(tolerance(r.res))
                }) =>
            {
                group.push(r);
            }
            _ => groups.push(vec![r]),
        }
    }
    // The newest group names the current window; a group a whole window before it (less the
    // wider tolerance) is the window that ended, not a disagreement about this one.
    let newest = groups.last()?;
    let newest_reset = newest.iter().map(|r| reset_of(r)).min()?;
    let newest_tol = newest.iter().map(|r| tolerance(r.res)).max()?;
    let earlier = |group: &Vec<&Reading>| {
        let reset = group.iter().map(|r| reset_of(r)).max().unwrap_or(0);
        let tol = group
            .iter()
            .map(|r| tolerance(r.res))
            .max()
            .unwrap_or(0)
            .max(newest_tol);
        match window_length(window) {
            Some(length) => newest_reset.saturating_sub(reset).saturating_add(tol) >= length,
            None => reset <= now && newest_reset > now,
        }
    };
    let overlapping: Vec<&Vec<&Reading>> = groups.iter().filter(|g| !earlier(g)).collect();
    // The window began a window's length before the earliest overlapping reset; a reset-less
    // reading made since belongs to it. With no fixed length, one made after the last reset an
    // earlier window named.
    let start = match window_length(window) {
        Some(length) => overlapping
            .iter()
            .flat_map(|g| g.iter())
            .map(|r| reset_of(r))
            .min()
            .map_or(0, |reset| reset.saturating_sub(length)),
        None => groups
            .iter()
            .filter(|g| earlier(g))
            .flat_map(|g| g.iter())
            .map(|r| reset_of(r))
            .max()
            .unwrap_or(0),
    };
    let fig = figure(
        overlapping
            .iter()
            .flat_map(|g| g.iter().copied())
            .chain(reset_less.iter().copied().filter(|r| r.t >= start)),
    )?;
    let reset = match overlapping.as_slice() {
        // The most precise reading's reset, the newest among equals.
        [one] => one
            .iter()
            .min_by_key(|r| (tolerance(r.res), std::cmp::Reverse(r.t)))
            .and_then(|r| r.reset),
        _ => None,
    };
    Some(Usage::Current {
        pct_x10: fig.pct_x10,
        reset,
        read: fig.t,
        by: fig.uuid.clone(),
        src: fig.src.clone(),
    })
}

/// The journal window as this Mac holds it at `now` (Unix seconds), read as a member, with this
/// Mac's heard archive from `since` on where `spool` names it; `None` where this Mac holds no
/// Commons it is a member of.
///
/// # Errors
/// A window table cannot be observed or read.
pub fn read(
    paths: &aterm_secrets::client::Paths,
    heard: Option<(&super::Spool, u64)>,
    now: u64,
) -> crate::Result<Option<Seen>> {
    read_started(paths, heard, now, std::time::Instant::now(), || {
        crate::accounts::claims::read(paths).ok()
    })
}

/// [`read`] with local custody admission's hang detector running from `started`, and `claims` for
/// the claims ring's read.
pub(super) fn read_started(
    paths: &aterm_secrets::client::Paths,
    heard: Option<(&super::Spool, u64)>,
    now: u64,
    started: std::time::Instant,
    claims: impl FnOnce() -> Option<crate::accounts::claims::Read>,
) -> crate::Result<Option<Seen>> {
    use crate::{catalog::Directory, table::Session};
    use aterm_secrets::commons::bootstrap::Bootstrap;
    let selected = crate::generation::selected(paths)?;
    let deadline = crate::admission::deadline_at(started);
    // The journal writer goes before the claims ring is read: that read takes `commons.lock`,
    // which an admission holds while it waits for this writer, so asking with the writer in hand
    // would be a lock-order cycle only the admission's hang detector ends.
    let mut seen = {
        let Ok((bootstrap, journal)) = Bootstrap::required_with_journal_until(&selected, deadline)
        else {
            return Ok(None);
        };
        let Ok(session) = Session::from_admitted_journal(&bootstrap, journal) else {
            return Ok(None);
        };
        let directory = Directory::new(&selected, bootstrap.trust());
        let window = super::flush::window(&session, &directory, now)?;
        let resolve = owned::commons::historical(&session);
        Seen::of(&window.ops, &resolve, now)
    };
    if let Some((spool, since)) = heard {
        // The archive is this Mac's own; one that cannot be read leaves the window's log.
        if let Ok(batches) = super::heard::batches(spool.dir(), since) {
            seen.hear(&batches);
        }
    }
    // The claims ring says who holds an account now; one that cannot be read leaves the
    // journal's answer.
    if let Some(claims) = claims() {
        seen.claim(&claims.rows);
    }
    Ok(Some(seen))
}

#[cfg(test)]
mod tests;
