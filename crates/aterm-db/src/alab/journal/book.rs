// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What this Mac's flusher knows and has said (design §4.17, §3.9): per account it used, the
//! facts its `use` row states (first and last use, sessions, sign-in state, the latest reading
//! and wall per window, the plan); the counter its records' keys and rows carry; what it last
//! wrote of each row and when; and the pace that keeps its records within its fixed share of a
//! week's table. Pure: `super::flush` loads it from `<spool>/book.json`, hands it the window's
//! tables and saves it.

use super::{
    event::{
        Event, Kind, MAX_EVENT, ModelBucket, Pk, SIGNIN_IN, SIGNIN_UNKNOWN, SIGNIN_WORDS, Source,
        Window, Word,
    },
    mine,
    place::{Holding, Why},
    relay::{Entry, Heard, Rk},
    spool::{KEEP_S, MAX_SPOOL},
    upd,
};
use crate::{
    Error, Result,
    catalog::ring::{JOURNAL_SHARES, Used},
    database::{Change, MAX_WRITES},
    typed::{
        Collection,
        owned::{caps, fit},
    },
};
use aterm_json::{Map, Value, json};
use aterm_update_core::update_record::UpdateRecord;
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

/// This Mac's sign-in word for an account: what its `use` row states.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SignIn {
    /// One of [`SIGNIN_WORDS`]. `unknown` is a word withdrawn at `t`: the book keeps it so an
    /// older event never restores what was withdrawn, and no row or reader is given it.
    pub state: &'static str,
    pub t: u64,
    pub cli: String,
}

impl SignIn {
    /// The word a row or a reader is given: none once it was withdrawn.
    pub(crate) fn stated(&self) -> Option<&Self> {
        (self.state != SIGNIN_UNKNOWN).then_some(self)
    }
}

/// What a row last said and when, so an unchanged row is never written again within a week.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Stated {
    pub value: Value,
    pub at: u64,
    pub period: u64,
}

/// A wall's key in the book: its window, and the model a `7d-model` wall is of
/// ([`ModelBucket`], [`Event::model`]), so two models' weekly walls stand apart and the clearing
/// of one clears no other. Every `7d-model` wall the book keeps names its model: one that names
/// none (an older build's note, still in this Mac's spool or in a book it saved) cannot say
/// whose week it is, so the book keeps no such wall and clears nothing by one
/// ([`Facts::fold`], [`Book::load`]); the harness notes each of its standing walls once more
/// with its model (`harness::journal::backfill`).
pub(crate) type WallKey = (Window, Option<ModelBucket>);

/// The key `e`'s wall or clearing is kept under ([`WallKey`]); `None` where it names no window,
/// or is a `7d-model` one that names no model.
fn wall_key(e: &Event) -> Option<WallKey> {
    let w = e.window?;
    (w != Window::WeekModel || e.model.is_some()).then_some((w, e.model))
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
    /// Each window's standing wall, a `7d-model` one by its model ([`WallKey`]): until, and when
    /// it was seen.
    pub walls: BTreeMap<WallKey, (Option<u64>, u64)>,
    pub plan: Option<(String, u64)>,
    /// The newest reading, wall or cleared wall, and where it was read: a `usage` row's `read_at`
    /// and `src` where it is newer than every reading the book keeps.
    pub read: Option<(u64, Source)>,
    /// On a sub-account's row, the provider's `AccountKey` it is keyed by (12 hex digits).
    pub key: Option<String>,
    /// On a sub-account's row, the account it belongs to: a sub-account key is a one-way digest
    /// of the two, so a reader joins the rows by this column and checks the key against both.
    pub account: Option<Pk>,
    /// The newest event about it.
    pub used: u64,
    pub stated: Option<Stated>,
    /// What its `usage` row last said ([`mine::content`]) and when.
    pub usage: Option<Stated>,
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
            Kind::Wall | Kind::WallCleared => {
                if let Some(key) = wall_key(e)
                    && self.walls.get(&key).is_some_and(|(_, t)| *t <= e.t)
                {
                    self.walls.remove(&key);
                }
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
        if let Some(state) = e.signin()
            && self.signin.as_ref().is_none_or(|s| s.t <= e.t)
        {
            self.signin = Some(SignIn {
                state,
                t: e.t,
                cli: e.cli.as_str().to_owned(),
            });
        }
        if ((e.kind == Kind::Quota && e.pct_x10.is_some())
            || matches!(e.kind, Kind::Wall | Kind::WallCleared))
            && e.window.is_some()
            && super::READING_SOURCES.contains(&e.src.word())
            && self.read.is_none_or(|(t, _)| t <= e.t)
        {
            self.read = Some((e.t, e.src));
        }
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
            Kind::Quota => {
                if let (Some(w), Some(_)) = (e.window, e.pct_x10)
                    && self.quota.get(&w).is_none_or(|held| held.t <= e.t)
                {
                    self.quota.insert(w, *e);
                }
            }
            Kind::Wall => {
                if let Some(key) = wall_key(e)
                    && self.walls.get(&key).is_none_or(|(_, t)| *t <= e.t)
                {
                    self.walls.insert(key, (e.until, e.t));
                }
            }
            Kind::WallCleared => {
                if let Some(key) = wall_key(e)
                    && self.walls.get(&key).is_some_and(|(_, t)| *t <= e.t)
                {
                    self.walls.remove(&key);
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

    /// The session starts it holds in the 7 days before `now`: a `use` row's `sessions_7d`.
    pub(crate) fn sessions_7d(&self, now: u64) -> usize {
        self.starts
            .iter()
            .filter(|t| t.saturating_add(7 * DAY) > now)
            .count()
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
        base.insert(
            "sessions_7d".into(),
            Value::from(self.sessions_7d(now) as u64),
        );
        if let Some(s) = self.signin.as_ref().and_then(SignIn::stated) {
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
        let wall = |(w, model): &WallKey, until: &Option<u64>, t: &u64| {
            let mut entry = Map::new();
            entry.insert("w".into(), Value::from(w.word()));
            if let Some(model) = model {
                entry.insert("m".into(), Value::from(model.word()));
            }
            entry.insert("until".into(), json!(until));
            entry.insert("t".into(), Value::from(*t));
            json!({ "wall": entry })
        };
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

    /// The content of the `usage` row (docs/DESIGN-shared-files-2026-11-11.md §10 file 4) of the
    /// account or sub-account `pk` at `now`, without its counter and the time it is stated
    /// ([`mine::row`]): each window's latest reading (tenths of a percent, the provider's reset
    /// in milliseconds, how exactly that reset is known), the standing wall of the whole account (5h, 7d or credits) that lasts
    /// longest (`wall`, `wall_until`), each model's own standing weekly wall by its bucket
    /// (`model_walls`, FL-269), the plan where it is a plan word
    /// the registry seeds ([`mine::plan_word`]), where and when the
    /// newest reading was read, and the sessions. `None` where nothing was read of it (no
    /// reading and no wall from a reading source), or a sub-account's row names no account.
    pub(crate) fn usage_row(&self, pk: &Pk, now: u64) -> Option<Value> {
        let (account, sub) = match (&self.account, &self.key) {
            (Some(account), Some(key)) => (*account, Some(key.as_str())),
            (None, None) if pk.as_str().starts_with("ac-") => (*pk, None),
            _ => return None,
        };
        let (read_at, src) = self
            .quota
            .values()
            .filter(|e| super::READING_SOURCES.contains(&e.src.word()))
            .map(|e| (e.t, e.src))
            .chain(self.read)
            .max_by_key(|(t, _)| *t)?;
        let mut row = Map::new();
        row.insert("r".into(), Value::from(VERSION));
        row.insert("account".into(), Value::from(account.as_str()));
        if let Some(sub) = sub {
            row.insert("sub".into(), Value::from(sub));
        }
        for (window, pct, reset, res) in [
            (Window::FiveHour, "w5h", "w5h_reset", "w5h_res"),
            (Window::Week, "w7d", "w7d_reset", "w7d_res"),
            (Window::WeekModel, "w7dm", "w7dm_reset", "w7dm_res"),
        ] {
            let Some(e) = self.quota.get(&window) else {
                continue;
            };
            if let Some(p) = e.pct_x10 {
                row.insert(pct.into(), Value::from(u64::from(p)));
            }
            if let Some(r) = e.reset {
                row.insert(reset.into(), Value::from(mine::ms(r)));
                if let Some(rr) = e.res {
                    row.insert(res.into(), Value::from(rr.word()));
                }
            }
        }
        // The standing account-wide wall that lasts longest; one with no stated end outlasts
        // every other. A model's own weekly wall walls that model's sessions alone, so it is no
        // wall of the account's: each stands in `model_walls` under its model's bucket.
        let standing = || {
            self.walls
                .iter()
                .filter(|(_, (until, _))| until.is_none_or(|u| u > now))
        };
        let wall = standing()
            .filter_map(|((w, _), (until, _))| Some((wall_word(*w)?, *until)))
            .max_by_key(|(_, until)| until.unwrap_or(u64::MAX));
        row.insert(
            "wall".into(),
            Value::from(wall.map_or(mine::Wall::None, |(word, _)| word).word()),
        );
        if let Some(until) = wall.and_then(|(_, until)| until) {
            row.insert("wall_until".into(), Value::from(mine::ms(until)));
        }
        // In the byte order of the bucket's word, as `ai/limits` folds them.
        let model_walls: Vec<Value> = standing()
            .filter_map(|((w, model), (until, _))| {
                let model = model.filter(|_| *w == Window::WeekModel)?;
                let mut entry = Map::new();
                entry.insert("bucket".into(), Value::from(model.word()));
                if let Some(until) = until {
                    entry.insert("until".into(), Value::from(mine::ms(*until)));
                }
                Some((model.word(), Value::Object(entry)))
            })
            .collect::<BTreeMap<_, _>>()
            .into_values()
            .collect();
        if !model_walls.is_empty() {
            row.insert("model_walls".into(), Value::Array(model_walls));
        }
        if let Some((word, _)) = self.plan.as_ref().filter(|(word, _)| mine::plan_word(word)) {
            row.insert("plan".into(), Value::from(word.as_str()));
        }
        row.insert("src".into(), Value::from(src.word()));
        row.insert("read_at".into(), Value::from(mine::ms(read_at)));
        row.insert(
            "sessions_7d".into(),
            Value::from(self.sessions_7d(now) as u64),
        );
        if self.last_start > 0 {
            row.insert("last_start".into(), Value::from(mine::ms(self.last_start)));
        }
        if self.last_end > 0 {
            row.insert("last_end".into(), Value::from(mine::ms(self.last_end)));
        }
        Some(Value::Object(row))
    }
}

/// A usage row's `wall` word of a window's wall, where it walls the account: none for a model's
/// own week (`7d-model`, stated in `model_walls` instead) and for the journal's own `day` and
/// `month` windows, which `ai/usage` does not name.
const fn wall_word(window: Window) -> Option<mine::Wall> {
    match window {
        Window::FiveHour => Some(mine::Wall::FiveHour),
        Window::Week => Some(mine::Wall::Week),
        Window::Credits => Some(mine::Wall::Credits),
        Window::WeekModel | Window::Day | Window::Month => None,
    }
}

/// One login this Mac's harness reports ([`Book::take_logins`]): its row's content as the
/// report says it ([`mine::Login::content`], the `place` reason applied when its row is made,
/// [`Book::login_content`]), when the harness last reported it, and what the row last said and
/// when.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct LoginFacts {
    pub content: Value,
    /// When the harness last reported it, Unix seconds: its row is restated in a new week only
    /// while that was this week or the last ([`Book::logins_due`]), so a store that stopped
    /// reading never has its old row restated as current.
    pub reported_at: u64,
    /// The report of its CLI no longer names it: `content` is its row as no longer held here
    /// ([`mine::released`]), stated once, then the book forgets it.
    pub gone: bool,
    pub stated: Option<Stated>,
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

fn same_content(stated: &Value, row: &Value) -> bool {
    let (Some(stated), Some(row)) = (stated.as_object(), row.as_object()) else {
        return false;
    };
    let row_len =
        row.len() - usize::from(row.contains_key("n")) - usize::from(row.contains_key("period"));
    stated.len() == row_len
        && stated
            .iter()
            .all(|(k, v)| !matches!(k.as_str(), "n" | "period") && row.get(k) == Some(v))
}

/// One account's facts as this Mac's book holds them, for a reader outside the flusher: the
/// metrics publisher's quota row (`docs/DESIGN-alab-one-mesh-2026-11-05.md` §4.14) takes its
/// sign-in word, last session start and end, sessions in 7 days and plan from here.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AccountFacts {
    /// The newest sign-in word (`in`, `out`, `ended`, `wrong`, `failed`) and when it was seen,
    /// Unix seconds.
    pub signin: Option<(&'static str, u64)>,
    /// The last session start here, Unix seconds; `None` before the first.
    pub last_start: Option<u64>,
    /// The last session end here, Unix seconds; `None` before the first.
    pub last_end: Option<u64>,
    /// Sessions started here in the 7 days before the read's `now`.
    pub sessions_7d: u32,
    /// The plan word and when it was seen.
    pub plan: Option<(String, u64)>,
}

/// One account this Mac holds that no preference here names, as the book holds it: why, since
/// when, and until when its row stands unless restated. An ended place has `until` at its end.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlaceFacts {
    /// Why this Mac holds it.
    pub why: Why,
    /// Since when it has held for that reason, Unix seconds.
    pub since: u64,
    /// Until when the place stands unless restated, Unix seconds.
    pub until: u64,
    /// The harness no longer reports it.
    pub ended: bool,
}

/// What this Mac's book knows of the accounts it used and the places it holds, by account key.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BookFacts {
    /// Each account's (or workspace's, under its `sa-` key) facts.
    pub accounts: BTreeMap<Pk, AccountFacts>,
    /// Each place.
    pub places: BTreeMap<Pk, PlaceFacts>,
}

impl Book {
    /// What the book knows, as of `now`.
    pub(crate) fn facts(&self, now: u64) -> BookFacts {
        let when = |t: u64| (t > 0).then_some(t);
        BookFacts {
            accounts: self
                .accounts
                .iter()
                .map(|(pk, facts)| {
                    (
                        *pk,
                        AccountFacts {
                            signin: facts
                                .signin
                                .as_ref()
                                .and_then(SignIn::stated)
                                .map(|s| (s.state, s.t)),
                            last_start: when(facts.last_start),
                            last_end: when(facts.last_end),
                            sessions_7d: u32::try_from(facts.sessions_7d(now)).unwrap_or(u32::MAX),
                            plan: facts.plan.clone(),
                        },
                    )
                })
                .collect(),
            places: self
                .places
                .iter()
                .map(|(pk, place)| {
                    (
                        *pk,
                        PlaceFacts {
                            why: place.why,
                            since: place.since,
                            until: place.until,
                            ended: place.ended,
                        },
                    )
                })
                .collect(),
        }
    }
}

/// What this Mac states about itself in its `sum` row (design §4.17), read by the caller.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Presence {
    /// Its membership label (`m22`).
    pub label: Option<Word<16>>,
    /// The macOS major version.
    pub os: Option<u64>,
}

/// One account this Mac holds that no preference here names (design §4.17 `place`): its row's
/// reason, since when it has held for that reason, until when the row stands unless restated,
/// and what the row last said.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Place {
    pub why: Why,
    pub since: u64,
    pub until: u64,
    /// The harness no longer reports it: the row is restated once with `until` at the end, then
    /// the book forgets it.
    pub ended: bool,
    pub stated: Option<Stated>,
}

/// When a live place row stated in week `period` lapses unless restated: the end of the next
/// week, so the weekly carry always restates it before it lapses, and a Mac that went away
/// leaves it live at most two weeks.
pub(crate) fn place_until(period: u64) -> u64 {
    period.saturating_add(2).saturating_mul(WEEK)
}

/// The flusher's state.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Book {
    pub accounts: BTreeMap<Pk, Facts>,
    /// What this Mac holds that no preference here names, by account ([`Self::hold`]).
    pub places: BTreeMap<Pk, Place>,
    /// The logins the harness last reported, by account ([`Self::take_logins`]).
    pub logins: BTreeMap<Pk, LoginFacts>,
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
    /// This Mac's update record as this pass read it ([`upd::record`]); never saved, so a pass
    /// that reads none writes no `upd` row.
    pub upd: Option<UpdateRecord>,
    /// What the last `upd` row said ([`upd::content`]) and when.
    pub upd_stated: Option<Stated>,
}

/// One planned flush record (design §4.17 "Record packing").
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Planned {
    pub changes: Vec<Change>,
    /// How many of the outbox's oldest events ride its `ev` batch.
    pub events: usize,
    pub sum: bool,
    pub uses: Vec<(Pk, Value)>,
    pub places: Vec<(Pk, Value)>,
    pub logins: Vec<(Pk, Value)>,
    pub usages: Vec<(Pk, Value)>,
    pub relays: Vec<(u8, Value)>,
    /// The `upd` row it carries.
    pub upd: Option<Value>,
    /// Planned with the journal off: its `sum` says so.
    pub off: bool,
}

/// `Command::Write::encode()` wraps the serialized `changes` JSON array (`[...]`) in a fixed
/// 78-byte envelope (`{"changes":...,"kind":"write","request":"<32-hex>","v":1}`), so an empty
/// write command is 80 bytes and each change adds its serialized JSON length (plus 1 comma byte
/// after the first change).
const EMPTY_WRITE_BYTES: usize = 80;

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
    /// journal was turned off or on, and with the journal on a sharp event, an update record the
    /// last `upd` row did not say ([`Self::upd_waiting`]), a login whose row changed urgently
    /// ([`Self::logins_urgent`]), 128 waiting events,
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
        outbox.iter().any(Event::sharp)
            || self.upd_waiting()
            || self.logins_urgent(now)
            || outbox.len() >= BATCH_EVENTS
            || ((owed || !outbox.is_empty()) && now >= self.flushed_at.saturating_add(ROUTINE_S))
    }

    /// The `use` rows due now: a row that changed and was last written an hour or more ago, and
    /// once a week the row of every account used in the last 180 days (the weekly carry).
    pub(crate) fn uses_due(&self, n: u64, period: u64, now: u64) -> Result<Vec<(Pk, Value)>> {
        let mut due = Vec::new();
        for (pk, facts) in &self.accounts {
            let said = facts.stated.as_ref();
            let carry = said.is_some_and(|s| s.period < period)
                && facts.used.saturating_add(CARRY_S) >= now;
            let eligible = said.is_none_or(|s| now >= s.at.saturating_add(USE_EVERY));
            if !carry && !eligible {
                continue;
            }
            let row = facts.use_row(n, period, now, caps::USE_ROW)?;
            let changed = eligible && said.is_none_or(|s| !same_content(&s.value, &row));
            if changed || carry {
                due.push((*pk, row));
            }
        }
        Ok(due)
    }

    /// Whether the update record this pass read says something the last `upd` row did not: due
    /// as promptly as a sharp event.
    pub(crate) fn upd_waiting(&self) -> bool {
        self.upd
            .as_ref()
            .and_then(|record| upd::row(record, 0, 0))
            .is_some_and(|row| {
                self.upd_stated
                    .as_ref()
                    .is_none_or(|said| said.value != upd::content(&row))
            })
    }

    /// The `upd` row due now at counter `n` in week `period`: the record says something the last
    /// row did not, or it is the first record of a week while a hold or a trial stands (the
    /// weekly carry; an idle, proven Mac's row lapses with its weeks).
    pub(crate) fn upd_due(&self, n: u64, period: u64) -> Option<Value> {
        let record = self.upd.as_ref()?;
        let row = upd::row(record, n, period)?;
        let said = self.upd_stated.as_ref();
        let changed = said.is_none_or(|said| said.value != upd::content(&row));
        let carry = said.is_some_and(|said| said.period < period)
            && (record.hold.is_some() || record.trial.is_some());
        (changed || carry).then_some(row)
    }

    /// Whether this Mac's journal saw `account` run or sign in here: a session start or a sign-in
    /// on its row. Until then a holding of it is [`Why::Probation`].
    fn proven(&self, account: &Pk) -> bool {
        self.accounts.get(account).is_some_and(|f| {
            f.last_start > 0 || f.signin.as_ref().is_some_and(|s| s.state == SIGNIN_IN)
        })
    }

    /// Take the harness's `holdings` at `now` (design §4.17 `place`): a new account, or one whose
    /// reason changed, holds since `now`; one whose journal shows no run or sign-in here is on
    /// [`Why::Probation`]; a held one stands until [`place_until`] of this week; one the harness no
    /// longer reports ends at `now` (stated once, then forgotten; one never stated is forgotten at
    /// once). At most [`MAX_ACCOUNTS`] are kept, the first reported.
    pub(crate) fn hold(&mut self, holdings: &[Holding], now: u64) {
        let until = place_until(now / WEEK);
        let mut held = BTreeSet::new();
        for holding in holdings.iter().take(MAX_ACCOUNTS) {
            if !held.insert(holding.account) {
                continue;
            }
            let why = if self.proven(&holding.account) {
                holding.why
            } else {
                Why::Probation
            };
            match self.places.get_mut(&holding.account) {
                Some(place) if !place.ended && place.why == why => place.until = until,
                Some(place) => {
                    place.why = why;
                    place.since = now;
                    place.until = until;
                    place.ended = false;
                }
                None => {
                    self.places.insert(
                        holding.account,
                        Place {
                            why,
                            since: now,
                            until,
                            ended: false,
                            stated: None,
                        },
                    );
                }
            }
        }
        self.places.retain(|account, place| {
            if held.contains(account) {
                return true;
            }
            if place.stated.is_none() {
                return false;
            }
            if !place.ended {
                place.ended = true;
                place.until = now.min(place.until);
            }
            true
        });
    }

    /// The `place` rows due now: a row that changed (a new reason, an end) and was last written an
    /// hour or more ago, and once a week each live row with its new `until` (the weekly carry).
    pub(crate) fn places_due(&self, n: u64, period: u64, now: u64) -> Vec<(Pk, Value)> {
        let mut due = Vec::new();
        for (pk, place) in &self.places {
            let said = place.stated.as_ref();
            let carry = !place.ended && said.is_some_and(|s| s.period < period);
            let eligible = said.is_none_or(|s| now >= s.at.saturating_add(USE_EVERY));
            if !carry && !eligible {
                continue;
            }
            let until = if place.ended {
                place.until
            } else {
                place.until.max(place_until(period))
            };
            let row = json!({"r": VERSION, "n": n, "why": place.why.word(),
                "since": place.since, "until": until});
            let changed = eligible && said.is_none_or(|s| !same_content(&s.value, &row));
            if changed || carry {
                due.push((*pk, row));
            }
        }
        due
    }

    /// Take the harness's `reports` at `now` (docs/DESIGN-shared-files-2026-11-11.md §10 file 3),
    /// one per CLI: each account's first reported login is kept with what its row last said,
    /// reported now. Of a CLI whose store read, a login the report no longer names is released
    /// ([`mine::released`]: stated once as no longer held here, then forgotten, as an ended
    /// `place` row is; one never stated is forgotten at once). Of a CLI whose store did not read,
    /// or the report names nothing of, every login stays as it was, its `reported_at` unmoved. A
    /// login whose row the value-free guard refuses ([`mine::checked`]) is not taken, not even
    /// into this book, which lives in the spool folder: it stays as the book held it, or unknown
    /// here, and is not released, so a word a reader should never have stated (a credential's
    /// fragment in a plan word) is kept nowhere. At most [`MAX_ACCOUNTS`] are kept: a new login
    /// past them is not taken.
    pub(crate) fn take_logins(&mut self, reports: &mine::Reports, now: u64) {
        let read: BTreeSet<&str> = reports
            .iter()
            .filter(|(_, logins)| logins.is_some())
            .map(|(cli, _)| cli.word())
            .collect();
        let mut reported = BTreeSet::new();
        for login in reports.values().flatten().flatten() {
            if !reported.insert(login.account) {
                continue;
            }
            let content = login.content();
            if mine::checked(&super::LOGIN, &mine::row(&content, 0, now)).is_err() {
                continue;
            }
            let room = self.logins.len() < MAX_ACCOUNTS;
            match self.logins.get_mut(&login.account) {
                Some(known) => {
                    known.content = content;
                    known.reported_at = now;
                    known.gone = false;
                }
                None if room => {
                    self.logins.insert(
                        login.account,
                        LoginFacts {
                            content,
                            reported_at: now,
                            gone: false,
                            stated: None,
                        },
                    );
                }
                None => {}
            }
        }
        self.logins.retain(|account, login| {
            let of_read = login
                .content
                .get("cli")
                .and_then(Value::as_str)
                .is_some_and(|cli| read.contains(cli));
            if reported.contains(account) || !of_read {
                return true;
            }
            if login.stated.is_none() {
                return false;
            }
            if !login.gone {
                login.gone = true;
                login.content = mine::released(&login.content, now);
            }
            true
        });
    }

    /// The content of `account`'s login row: what the harness reported, saying the reason this
    /// Mac's `place` row gives where it holds the account for one ([`Self::hold`], so `probation`
    /// until it ran or signed in here).
    pub(crate) fn login_content(&self, account: &Pk, login: &LoginFacts) -> Value {
        let mut content = login.content.clone();
        if !login.gone
            && let Some(place) = self.places.get(account).filter(|place| !place.ended)
            && let Some(row) = content.as_object_mut()
        {
            row.insert("held".into(), Value::from(mine::Held::of(place.why).word()));
        }
        content
    }

    /// Whether a login's row changed urgently since it was last stated ([`mine::urgent`]; one
    /// never stated, from nothing) and the value-free guard admits it, so a record is due for it
    /// as for a sharp event.
    pub(crate) fn logins_urgent(&self, now: u64) -> bool {
        self.logins.iter().any(|(account, login)| {
            let content = self.login_content(account, login);
            mine::urgent(
                login.stated.as_ref().map_or(&Value::Null, |s| &s.value),
                &content,
            ) && mine::checked(&super::LOGIN, &mine::row(&content, 0, now)).is_ok()
        })
    }

    /// Whether a row whose content is `content` and last said `said` is due at `now` in week
    /// `period`, and whether that is its weekly restatement: an urgent change ([`mine::urgent`])
    /// at once, any other change once an hour has passed since the row was last written, and a
    /// row last stated in an earlier week (where `carries`) in the week's first record.
    fn row_due(
        said: Option<&Stated>,
        content: &Value,
        carries: bool,
        period: u64,
        now: u64,
    ) -> bool {
        let carry = carries && said.is_some_and(|s| s.period < period);
        let eligible = said.is_none_or(|s| {
            now >= s.at.saturating_add(USE_EVERY) || mine::urgent(&s.value, content)
        });
        let changed = eligible && said.is_none_or(|s| s.value != *content);
        changed || carry
    }

    /// The `login` rows due now at counter `n` in week `period` ([`Self::row_due`]), each saying
    /// its `place` reason ([`Self::login_content`]): a login is restated in a new week while the
    /// harness reported it this week or the last, so a store that stopped reading lapses with its
    /// weeks rather than being restated as current; a released one is stated once and never
    /// restated.
    pub(crate) fn logins_due(&self, n: u64, period: u64, now: u64) -> Vec<(Pk, Value)> {
        self.logins
            .iter()
            .filter_map(|(pk, login)| {
                let content = self.login_content(pk, login);
                let carries = !login.gone && (login.reported_at / WEEK).saturating_add(1) >= period;
                Self::row_due(login.stated.as_ref(), &content, carries, period, now)
                    .then(|| (*pk, mine::row(&content, n, now)))
            })
            .collect()
    }

    /// The `usage` rows due now at counter `n` in week `period` ([`Self::row_due`]): one for each
    /// account or sub-account something was read of ([`Facts::usage_row`]), restated in every new
    /// week while its `use` row is (an account used in the last 180 days).
    pub(crate) fn usages_due(&self, n: u64, period: u64, now: u64) -> Vec<(Pk, Value)> {
        let mut due = Vec::new();
        for (pk, facts) in &self.accounts {
            let Some(content) = facts.usage_row(pk, now) else {
                continue;
            };
            let carries = facts.used.saturating_add(CARRY_S) >= now;
            if Self::row_due(facts.usage.as_ref(), &content, carries, period, now) {
                due.push((*pk, mine::row(&content, n, now)));
            }
        }
        due
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
            let said = self.relays.get(&bucket);
            let carry = said.is_some_and(|s| s.period < period);
            let eligible = said.is_none_or(|s| now >= s.at.saturating_add(ROUTINE_S));
            if !carry && !eligible {
                continue;
            }
            let Some(row) = heard.row(bucket, n, period, caps::RELAY_ROW)? else {
                continue;
            };
            let changed = eligible && said.is_none_or(|s| !same_content(&s.value, &row));
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
        row.insert("aterm".into(), Value::from(aterm_stamp::app_version()));
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
    /// §4.17 "Record packing"): the `ev` batch first, then `sum`, then the `upd` row, then the
    /// `login` and `usage` rows that changed urgently ([`mine::urgent`]), then the changed `use`
    /// rows, then the `place` rows, then the `relay` rows, then every other `login` and `usage`
    /// row due (a routine change, the weekly restatement), while the record stays within 64
    /// changes and 60 KiB; what does not fit goes in the next flush, so the rows builds before
    /// SF-08 read are never held back by the new ones. A `login` or `usage` row the value-free
    /// guard refuses ([`mine::checked`]) is never written. With the journal off (`off`) the record holds the
    /// `sum` row alone, saying so: no event, `upd`, `login`, `usage`, `use`, `place` or `relay`
    /// row is published.
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
        let mut payload_len = EMPTY_WRITE_BYTES;
        let outbox = if off { &[] } else { outbox };
        if !outbox.is_empty() {
            let values: Vec<Value> = outbox.iter().map(Event::to_value).collect();
            let batch = |kept: &[Value]| json!({"r": VERSION, "n": n, "e": kept});
            let events = fit(&values, caps::EV_BATCH, batch)?;
            if events > 0 {
                let change = Change {
                    collection: super::EV.name.into(),
                    key: format!("{uuid}.{n}"),
                    value: Some(batch(&values[..events])),
                };
                let change_len = aterm_json::to_vec(&change)
                    .map_err(|_| Error::new("a journal flush record passes its limits"))?
                    .len();
                payload_len += change_len;
                if payload_len > caps::RECORD {
                    return Err(Error::new("a journal flush record passes its limits"));
                }
                planned.changes.push(change);
                planned.events = events;
            }
        }
        let offer = |planned: &mut Planned, payload_len: &mut usize, change: Change| {
            if planned.changes.len() >= MAX_WRITES {
                return false;
            }
            let Some(change_len) = aterm_json::to_vec(&change).ok().map(|b| b.len()) else {
                return false;
            };
            let next_len = *payload_len + usize::from(!planned.changes.is_empty()) + change_len;
            if next_len > caps::RECORD {
                return false;
            }
            *payload_len = next_len;
            planned.changes.push(change);
            true
        };
        if self.sum_period != Some(period) || self.sum_off != off {
            let row = self.sum_row(presence, n, period, now, off);
            let change = Change {
                collection: super::SUM.name.into(),
                key: uuid.clone(),
                value: Some(row),
            };
            planned.sum = offer(&mut planned, &mut payload_len, change);
        }
        if off {
            return Ok(planned);
        }
        if let Some(row) = self.upd_due(n, period) {
            let change = Change {
                collection: super::UPD.name.into(),
                key: uuid.clone(),
                value: Some(row.clone()),
            };
            if offer(&mut planned, &mut payload_len, change) {
                planned.upd = Some(row);
            }
        }
        // Each `login` and `usage` row due, the guard's refusals left out, and whether it changed
        // urgently since it was last stated.
        let said = |collection: &Collection, pk: &Pk| {
            if collection.name == super::LOGIN.name {
                self.logins.get(pk).and_then(|l| l.stated.as_ref())
            } else {
                self.accounts.get(pk).and_then(|f| f.usage.as_ref())
            }
        };
        let (login, usage) = (&super::LOGIN, &super::USAGE);
        let mut mine_due: Vec<(&Collection, Pk, Value, bool)> = Vec::new();
        for (collection, due) in [
            (login, self.logins_due(n, period, now)),
            (usage, self.usages_due(n, period, now)),
        ] {
            for (pk, row) in due {
                if mine::checked(collection, &row).is_err() {
                    continue;
                }
                let urgent = mine::urgent(
                    said(collection, &pk).map_or(&Value::Null, |s| &s.value),
                    &mine::content(&row),
                );
                mine_due.push((collection, pk, row, urgent));
            }
        }
        let offer_mine =
            |planned: &mut Planned,
             payload_len: &mut usize,
             (collection, pk, row): (&Collection, Pk, Value)| {
                let change = Change {
                    collection: collection.name.into(),
                    key: format!("{}.{uuid}", pk.as_str()),
                    value: Some(row.clone()),
                };
                if offer(planned, payload_len, change) {
                    if collection.name == super::LOGIN.name {
                        planned.logins.push((pk, row));
                    } else {
                        planned.usages.push((pk, row));
                    }
                }
            };
        let (urgent, routine): (Vec<_>, Vec<_>) =
            mine_due.into_iter().partition(|(_, _, _, urgent)| *urgent);
        for (collection, pk, row, _) in urgent {
            offer_mine(&mut planned, &mut payload_len, (collection, pk, row));
        }
        for (pk, row) in self.uses_due(n, period, now)? {
            let change = Change {
                collection: super::USE.name.into(),
                key: format!("{}.{uuid}", pk.as_str()),
                value: Some(row.clone()),
            };
            if offer(&mut planned, &mut payload_len, change) {
                planned.uses.push((pk, row));
            }
        }
        for (pk, row) in self.places_due(n, period, now) {
            let change = Change {
                collection: super::PLACE.name.into(),
                key: format!("{}.{uuid}", pk.as_str()),
                value: Some(row.clone()),
            };
            if offer(&mut planned, &mut payload_len, change) {
                planned.places.push((pk, row));
            }
        }
        for (bucket, row) in self.relays_due(heard, me, n, period, now)? {
            let change = Change {
                collection: super::RELAY.name.into(),
                key: format!("rl-{bucket:x}.{uuid}"),
                value: Some(row.clone()),
            };
            if offer(&mut planned, &mut payload_len, change) {
                planned.relays.push((bucket, row));
            }
        }
        for (collection, pk, row, _) in routine {
            offer_mine(&mut planned, &mut payload_len, (collection, pk, row));
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
        for (pk, row) in &planned.places {
            let ended = self.places.get_mut(pk).is_some_and(|place| {
                place.stated = Some(Stated {
                    value: content(row),
                    at: now,
                    period,
                });
                place.ended
            });
            if ended {
                self.places.remove(pk);
            }
        }
        for (pk, row) in &planned.logins {
            let gone = self.logins.get_mut(pk).is_some_and(|login| {
                login.stated = Some(Stated {
                    value: mine::content(row),
                    at: now,
                    period,
                });
                login.gone
            });
            if gone {
                self.logins.remove(pk);
            }
        }
        for (pk, row) in &planned.usages {
            if let Some(facts) = self.accounts.get_mut(pk) {
                facts.usage = Some(Stated {
                    value: mine::content(row),
                    at: now,
                    period,
                });
            }
        }
        if let Some(row) = &planned.upd {
            self.upd_stated = Some(Stated {
                value: upd::content(row),
                at: now,
                period,
            });
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
                            .map(|((w, model), (u, t))| {
                                let mut wall = Map::new();
                                wall.insert("w".into(), Value::from(w.word()));
                                if let Some(model) = model {
                                    wall.insert("m".into(), Value::from(model.word()));
                                }
                                wall.insert("until".into(), json!(u));
                                wall.insert("t".into(), Value::from(*t));
                                Value::Object(wall)
                            })
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
                if let Some((t, src)) = f.read {
                    o.insert("read".into(), json!({"t": t, "src": src.word()}));
                }
                if let Some(s) = &f.usage {
                    o.insert("usage".into(), stated(s));
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
            "places": self.places.iter()
                .map(|(pk, p)| {
                    let mut o = Map::new();
                    o.insert("why".into(), Value::from(p.why.word()));
                    o.insert("since".into(), Value::from(p.since));
                    o.insert("until".into(), Value::from(p.until));
                    o.insert("ended".into(), Value::from(p.ended));
                    if let Some(s) = &p.stated {
                        o.insert("stated".into(), stated(s));
                    }
                    (pk.as_str().to_owned(), Value::Object(o))
                })
                .collect::<Map>(),
            "logins": self.logins.iter()
                .map(|(pk, login)| {
                    let mut o = Map::new();
                    o.insert("content".into(), login.content.clone());
                    o.insert("reported_at".into(), Value::from(login.reported_at));
                    if login.gone {
                        o.insert("gone".into(), Value::Bool(true));
                    }
                    if let Some(s) = &login.stated {
                        o.insert("stated".into(), stated(s));
                    }
                    (pk.as_str().to_owned(), Value::Object(o))
                })
                .collect::<Map>(),
            "relays": self.relays.iter()
                .map(|(b, s)| (b.to_string(), stated(s)))
                .collect::<Map>(),
            "upd": self.upd_stated.as_ref().map(stated),
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
            upd_stated: stated(v.get("upd")),
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
                read: f.get("read").and_then(|r| {
                    Some((
                        r.get("t")?.as_u64()?,
                        Source::parse(r.get("src")?.as_str()?)?,
                    ))
                }),
                usage: stated(f.get("usage")),
                ..Facts::default()
            };
            facts.signin = f.get("signin").and_then(|s| {
                let word = s.get("state")?.as_str()?;
                Some(SignIn {
                    state: SIGNIN_WORDS.into_iter().find(|w| *w == word)?,
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
                    // A model only on a `7d-model` wall, which always names one: a book saved
                    // before walls named their model holds such a wall with none, and that is
                    // dropped ([`WallKey`]).
                    let model = w
                        .get("m")
                        .and_then(Value::as_str)
                        .and_then(ModelBucket::parse)
                        .filter(|_| window == Window::WeekModel);
                    if window == Window::WeekModel && model.is_none() {
                        continue;
                    }
                    facts
                        .walls
                        .insert((window, model), (w.get("until").and_then(Value::as_u64), t));
                }
            }
            facts.plan = f
                .get("plan")
                .and_then(|p| Some((p.get("word")?.as_str()?.to_owned(), p.get("t")?.as_u64()?)));
            book.accounts.insert(pk, facts);
        }
        for (pk, p) in v
            .get("places")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
        {
            let parsed = (|| {
                Some((
                    Pk::new(pk)?,
                    Place {
                        why: Why::parse(p.get("why")?.as_str()?)?,
                        since: p.get("since")?.as_u64()?,
                        until: p.get("until")?.as_u64()?,
                        ended: p.get("ended")?.as_bool()?,
                        stated: stated(p.get("stated")),
                    },
                ))
            })();
            if let Some((pk, place)) = parsed {
                book.places.insert(pk, place);
            }
        }
        for (pk, login) in v
            .get("logins")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
        {
            if let (Some(pk), Some(content)) =
                (Pk::new(pk), login.get("content").filter(|c| c.is_object()))
            {
                book.logins.insert(
                    pk,
                    LoginFacts {
                        content: content.clone(),
                        reported_at: login
                            .get("reported_at")
                            .and_then(Value::as_u64)
                            .unwrap_or_default(),
                        gone: login.get("gone").and_then(Value::as_bool) == Some(true),
                        stated: stated(login.get("stated")),
                    },
                );
            }
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
