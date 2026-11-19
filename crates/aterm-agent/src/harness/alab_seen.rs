// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The journal-backed account surfaces (`docs/DESIGN-alab-lab-model-2026-11-01.md` §7, §8, step
//! 11): who last used an account and on which Mac, where it is signed in, its quota per window
//! with the reading's age, Mac and source, and its history across Macs.
//!
//! `aterm-agent` does not link `aterm-db`, so the journal reaches it as a [`Seen`] the `aterm`
//! executable reads in the background from the journal window and the registry's LabView
//! (`crates/aterm/src/alab_seen.rs`) and hands over through a [`Source`] it installs at start, as
//! the lab's roster source is. Nothing here calls a provider or writes anything. A surface on a
//! typing or painting path asks with `wait: false` and gets what is held, or nothing yet; a verb a
//! person ran (`aterm harness accounts`, `accounts history`) waits for one read. With no source
//! installed (a test, a debug build, `[alab] roster = false`) there is nothing to show and every
//! surface shows what this Mac's own records hold, as before.

use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};

/// A Mac as the journal names it: its machine label where the registry binds its Commons
/// enrollment to a live machine, else `uuid:<8 hex>` of the enrollment.
pub type Mac = String;

/// One account's last use.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LastUse {
    pub by: Mac,
    /// When, by that Mac's clock (Unix seconds).
    pub t: i64,
}

/// One Mac's latest sign-in word for an account.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignedIn {
    pub on: Mac,
    /// `in`, `out`, `ended`, `wrong` or `failed`.
    pub state: String,
    pub t: i64,
}

/// What one window's readings say.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Figure {
    /// The most used of the current window's readings; `reset` is `None` where the readings
    /// disagree on it beyond their resolution.
    Used { pct_x10: u16, reset: Option<i64> },
    /// Every reset read has passed and nothing was read since.
    ResetUnread { reset: i64 },
}

/// One window of an account (or of one of its workspaces), or one model's own week of its
/// `7d-model` window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowSeen {
    /// `5h`, `7d`, `7d-model`, `day`, `month` or `credits`.
    pub window: String,
    /// The workspace's sub-account key where the window is a workspace's.
    pub sub: Option<String>,
    /// The weekly bucket of the model whose own week this is ([`super::profile::Bucket::id`]:
    /// `seven_day_overage_included` for Fable, `seven_day_opus`, `seven_day_sonnet`,
    /// `seven_day_other`), for a `7d-model` wall the journal names the model of (FL-271): such a
    /// window holds that model's wall alone. `None` for the window's own, which holds its
    /// figure and, in `7d-model`, an older build's wall that names no model.
    pub model: Option<String>,
    pub figure: Option<Figure>,
    /// Original time, Mac and source of the quota reading retained with this window.
    pub read: Option<(i64, Mac, String)>,
    /// The Commons uuid (32 hex) of the Mac that made [`Self::read`]: whether it is this Mac's
    /// own ([`Seen::me`]) is told by it, never by a name.
    pub reader: Option<String>,
    /// A wall standing at the read, until when.
    pub wall_until: Option<Option<i64>>,
    /// When the standing wall was seen, the Mac that saw it, and that Mac's Commons uuid.
    pub wall_by: Option<(i64, Mac, String)>,
}

/// The journal's word of the models' weeks' window.
pub const WEEK_MODEL: &str = "7d-model";

impl WindowSeen {
    /// Whether this window is one every session of the account meets, so its wall walls them
    /// all and its figure is the account's: any window but `7d-model`, or the week of a model
    /// with no week of its own here (`seven_day_other`, or a bucket this build does not know),
    /// which the harness takes as the account's ([`super::accounts::model_window`],
    /// `Account::limited_until`). A Fable, Opus or Sonnet week's wall walls that model's
    /// sessions alone, and the `7d-model` window's own row cannot say whose week its wall (an
    /// older build's) or its figure (the most used of the models' weeks: a reading names no
    /// model) is of, so neither is the account's (FL-271).
    #[must_use]
    pub fn walls_account(&self) -> bool {
        match &self.model {
            Some(bucket) => !super::accounts::model_window(bucket),
            None => self.window != WEEK_MODEL,
        }
    }

    /// What a line calls this window: its word (`5h`, `7d`, `7d-model`), or for a model's own
    /// week that week's words (`Fable week`, `Opus week`, `Sonnet week`; `another model's week`
    /// for one no bucket of this build names).
    #[must_use]
    pub fn name(&self) -> String {
        match &self.model {
            Some(bucket) => super::accounts::model_week_name(bucket)
                .unwrap_or("another model's week")
                .to_owned(),
            None => self.window.clone(),
        }
    }

    /// Project this retained reading at `now`: a passed wall is lifted and a passed reset
    /// becomes unknown until read again. Unknown bounds and the original reading are kept.
    #[must_use]
    pub fn as_of(&self, now: i64) -> Self {
        let mut window = self.clone();
        if matches!(window.wall_until, Some(Some(until)) if until <= now) {
            window.wall_until = None;
        }
        if let Some(Figure::Used {
            reset: Some(reset), ..
        }) = window.figure
            && reset <= now
        {
            window.figure = Some(Figure::ResetUnread { reset });
        }
        window
    }
}

/// One line of an account's history across Macs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    pub t: i64,
    pub by: Mac,
    /// The journal's event word (`session-start`, `signed-in`, `quota`, …).
    pub kind: String,
    pub cli: String,
    /// The closed words the event carries (window, figure, reason), never free text.
    pub detail: String,
    /// Read from the reading Mac's heard archive rather than a window table.
    pub heard: bool,
    /// The window the event is of (`5h`, `7d`), where it names one.
    pub window: Option<String>,
    /// The window is a workspace's own, not the account's.
    pub sub: bool,
    /// The window's tenths of a percent used, where the event carries a figure.
    pub pct_x10: Option<u16>,
    /// The window's reset (Unix seconds), where the event names it.
    pub reset: Option<i64>,
}

/// What the journal says of one account.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AccountSeen {
    pub last_use: Option<LastUse>,
    pub signed_in: Vec<SignedIn>,
    pub windows: Vec<WindowSeen>,
    /// Its events, oldest first, at most [`HISTORY_MAX`].
    pub history: Vec<Line>,
}

/// The most history lines one account keeps in memory.
pub const HISTORY_MAX: usize = 512;

/// What the journal says, by account key (`ac-…`), as of [`Self::at`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Seen {
    pub accounts: BTreeMap<String, AccountSeen>,
    /// When it was read (Unix seconds).
    pub at: i64,
    /// This Mac's own Commons uuids (32 hex): its seat's, the seats it succeeded and each the
    /// registry binds to this machine. A fact one of them made is this Mac's own, which its own
    /// records already hold, and never a peer's ([`bound_of`]).
    pub me: std::collections::BTreeSet<String>,
}

impl Seen {
    /// Whether `other` says what this says: the same accounts and the same own uuids, whatever
    /// instant each was read at ([`Self::at`]), so a re-read that found nothing new is no move.
    #[must_use]
    pub fn says_the_same(&self, other: &Self) -> bool {
        self.accounts == other.accounts && self.me == other.me
    }
}

/// What gives the harness its [`Seen`]: the `aterm` executable's cache, which keeps what it
/// read with [`hold`], so a change of what it holds is told to the one [`listen`]er.
pub trait Source: Send + Sync {
    /// The journal as last read. `wait`: read once now when nothing is held yet (a verb a person
    /// ran); otherwise answer at once with what is held, starting a background read if none is.
    fn seen(&self, wait: bool) -> Option<Arc<Seen>>;
}

static INSTALLED: OnceLock<Box<dyn Source>> = OnceLock::new();

/// Install the process's [`Source`], once; a second install is refused.
pub fn install(source: Box<dyn Source>) -> bool {
    INSTALLED.set(source).is_ok()
}

/// What [`hold`] calls when the journal held comes to say something else.
static LISTENER: OnceLock<fn()> = OnceLock::new();

/// Name the process's one listener, called when the journal a [`Source`] holds comes to say
/// something else ([`hold`]), so a reader of [`journal`] that must not poll it (a footer that
/// folds it) hears the move instead; a second listener is refused.
pub fn listen(moved: fn()) -> bool {
    LISTENER.set(moved).is_ok()
}

/// Keep `fresh` in `slot`, a [`Source`]'s held journal, and tell the [`listen`]er where it says
/// something else than what `slot` held: a first read, one dropped (a policy turned the lab off)
/// or an account's change; a re-read that says the same at a later instant
/// ([`Seen::says_the_same`]) tells nothing. Answers whether the journal held moved, which is
/// whether it told where [`listen`] named a listener; in a process that named none, nothing is
/// told and the answer is the same.
pub fn hold(slot: &mut Option<Arc<Seen>>, fresh: Option<Arc<Seen>>) -> bool {
    let moved = match (slot.as_deref(), fresh.as_deref()) {
        (None, None) => false,
        (Some(held), Some(fresh)) => !held.says_the_same(fresh),
        _ => true,
    };
    *slot = fresh;
    if moved && let Some(listener) = LISTENER.get() {
        listener();
    }
    moved
}

#[cfg(test)]
thread_local! {
    static OVERRIDE: std::cell::RefCell<Option<Arc<Seen>>> = const { std::cell::RefCell::new(None) };
}

/// The journal as this process holds it ([`Source::seen`]); `None` with no source installed.
#[must_use]
pub fn current(wait: bool) -> Option<Arc<Seen>> {
    #[cfg(test)]
    if let Some(seen) = OVERRIDE.with(|cell| cell.borrow().clone()) {
        return Some(seen);
    }
    INSTALLED.get()?.seen(wait)
}

/// Whether this process reads the journal, and what it holds: the three states a surface that
/// must not wait tells apart ([`journal`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Journal {
    /// No source is installed (a test, a debug build, `[alab] roster = false`): this process
    /// reads no journal, now or later.
    Off,
    /// A source is installed and holds nothing yet; asking started its background read.
    NotReadYet,
    /// The journal as last read.
    Read(Arc<Seen>),
}

/// The journal as this process holds it now, never waiting for a read ([`current`] with
/// `wait: false`), with why there is none: [`Journal::Off`] says nothing will come,
/// [`Journal::NotReadYet`] that a later ask may hold it.
#[must_use]
pub fn journal() -> Journal {
    #[cfg(test)]
    if let Some(seen) = OVERRIDE.with(|cell| cell.borrow().clone()) {
        return Journal::Read(seen);
    }
    match INSTALLED.get() {
        None => Journal::Off,
        Some(source) => source
            .seen(false)
            .map_or(Journal::NotReadYet, Journal::Read),
    }
}

/// The journal's event word for a session started on an account ([`Line::kind`]).
pub const SESSION_START: &str = "session-start";

/// The journal's event word of a quota reading.
pub const QUOTA: &str = "quota";

/// The journal's word of the week's window.
pub const WEEK: &str = "7d";

/// The account's own week as the lab's Macs read it, over the history lines held: each
/// [`QUOTA`] line of the [`WEEK`] window that carries a figure and its reset, as (when, tenths
/// used, reset), oldest first. A workspace's window is not the account's, and a figure with no
/// reset names no week to draw it in: neither is taken.
#[must_use]
pub fn week_reads(account: &AccountSeen) -> Vec<(i64, u16, i64)> {
    account
        .history
        .iter()
        .filter(|line| line.kind == QUOTA && !line.sub && line.window.as_deref() == Some(WEEK))
        .filter_map(|line| Some((line.t, line.pct_x10?, line.reset?)))
        .collect()
}

/// How many sessions each Mac started on `account`, over the history lines held: each Mac with
/// its count of [`SESSION_START`] lines, most first (a tie by name), and the oldest held line's
/// time, of any kind. The history is the newest [`HISTORY_MAX`] lines, so the counts are of
/// starts since that instant and of no fixed span; `None` where it holds no start.
#[must_use]
pub fn starts_by_mac(account: &AccountSeen) -> Option<(Vec<(Mac, u32)>, i64)> {
    let mut by: BTreeMap<&str, u32> = BTreeMap::new();
    for line in account
        .history
        .iter()
        .filter(|line| line.kind == SESSION_START)
    {
        let n = by.entry(line.by.as_str()).or_default();
        *n = n.saturating_add(1);
    }
    let since = account.history.iter().map(|line| line.t).min()?;
    if by.is_empty() {
        return None;
    }
    let mut starts: Vec<(Mac, u32)> = by.into_iter().map(|(mac, n)| (mac.to_owned(), n)).collect();
    starts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    Some((starts, since))
}

/// Run `f` with `seen` as this thread's [`current`] journal.
#[cfg(test)]
pub(crate) fn with_seen<T>(seen: Seen, f: impl FnOnce() -> T) -> T {
    OVERRIDE.with(|cell| *cell.borrow_mut() = Some(Arc::new(seen)));
    let out = f();
    OVERRIDE.with(|cell| *cell.borrow_mut() = None);
    out
}

/// `seconds` as a short age: `40 s`, `12 min`, `3 h`, `2 d`.
#[must_use]
pub fn age(seconds: i64) -> String {
    duration_words(u64::try_from(seconds).unwrap_or_default())
}

fn duration_words(seconds: u64) -> String {
    match seconds {
        0..60 => format!("{seconds} s"),
        60..3_600 => format!("{} min", seconds / 60),
        3_600..172_800 => format!("{} h", seconds / 3_600),
        _ => format!("{} d", seconds / 86_400),
    }
}

/// "m17, about 3 h ago (its clock)", or "… ahead (its clock)" for a time past `now`.
#[must_use]
pub fn when(by: &str, t: i64, now: i64) -> String {
    let age = duration_words(now.abs_diff(t));
    if t > now {
        format!("{by}, {age} ahead (its clock)")
    } else {
        format!("{by}, about {age} ago (its clock)")
    }
}

/// The retained quota reading's original Mac, source and source-clock age or lead.
#[must_use]
pub fn read_words(t: i64, by: &str, source: &str, now: i64) -> String {
    format!("read on {} ({source})", when(by, t, now))
}

/// Each Mac's retained sign-in word, with its age or clock lead at the projection time.
/// These records do not establish a current provider login.
#[must_use]
pub fn signed_in_words(records: &[SignedIn], now: i64) -> Option<String> {
    (!records.is_empty()).then(|| {
        records
            .iter()
            .map(|record| when(&format!("{} ({})", record.on, record.state), record.t, now))
            .collect::<Vec<_>>()
            .join("; ")
    })
}

/// One window as a line: what is left of it ([`super::left`], to the tenth the journal holds of
/// its use), its reset, reading provenance and standing wall. A reading whose source clock is
/// ahead says so; an unread reset is never shown as a full window left.
#[must_use]
pub fn window_line(w: &WindowSeen, now: i64) -> String {
    let w = w.as_of(now);
    let mut out = match &w.sub {
        Some(sub) => format!("{} (workspace {sub})", w.name()),
        None => w.name(),
    };
    match &w.figure {
        Some(Figure::Used { pct_x10, reset }) => {
            out.push_str(&format!(
                " {}% {}",
                super::left::tenths(u32::from(*pct_x10)),
                super::left::WORD
            ));
            match reset {
                Some(reset) => {
                    out.push_str(&format!(", resets {}", super::usage::rfc3339_utc(*reset)));
                }
                None => out.push_str(", reset unknown"),
            }
        }
        Some(Figure::ResetUnread { reset }) => out.push_str(&format!(
            " reset at {}, not read since",
            super::usage::rfc3339_utc(*reset)
        )),
        None => {}
    }
    if let Some((t, by, src)) = &w.read {
        out.push_str(&format!(", {}", read_words(*t, by, src, now)));
    }
    if let Some(until) = w.wall_until {
        match until {
            Some(until) => out.push_str(&format!(
                ", at its limit until {}",
                super::usage::rfc3339_utc(until)
            )),
            None => out.push_str(", at its limit"),
        }
    }
    out
}

/// What the journal says of `account` as lines, for a report: last used by, sign-in records,
/// and each window. Empty where it says nothing.
#[must_use]
pub fn lines(account: &AccountSeen, now: i64) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(last) = &account.last_use {
        out.push(format!("last used by {}", when(&last.by, last.t, now)));
    }
    if let Some(records) = signed_in_words(&account.signed_in, now) {
        out.push(format!("sign-in records: {records}"));
    }
    out.extend(account.windows.iter().map(|w| window_line(w, now)));
    out
}

/// How far ahead of this Mac's clock a peer's reading may be dated and still be admitted
/// ([`bound_of`]): the clock-skew bound the claims ring reads a Mac's clock as split past.
pub const BOUND_AHEAD_S: i64 = 900;

/// How far past a whole window from now a peer's reset may lie and still name the window now
/// ([`bound_of`]): the skew bound plus the coarsest a reset is read to (an hour, "Resets 1pm").
pub const BOUND_RESET_SLACK_S: i64 = 3_600;

/// The journal's word of the five-hour window.
pub const FIVE: &str = "5h";

/// What a peer's evidence admits of one window, for display beside a row whose own reading is
/// unknown or whose reset passed ([`bound_of`]). A peer's reading is evidence the account was
/// used, never that it has room: a figure is a lower bound of use until its reset, since any Mac
/// may have spent the window since. Nothing here enters the accounts state, a pick, a launch, an
/// advice or a file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Bound {
    /// At least `pct_x10` tenths used until `reset`, read at `read` (never after the read's own
    /// instant) on `by` from `src`.
    Used {
        pct_x10: u16,
        reset: i64,
        read: i64,
        by: Mac,
        src: String,
    },
    /// `by` saw the window at its limit at `read`, standing until `until`.
    Wall { until: i64, read: i64, by: Mac },
}

impl Bound {
    /// The instant the bound holds to: the reading's reset, or the wall's end.
    #[must_use]
    pub const fn until(&self) -> i64 {
        match self {
            Bound::Used { reset, .. } => *reset,
            Bound::Wall { until, .. } => *until,
        }
    }

    /// The tenths used it bounds: the reading's figure, a whole window for a wall.
    #[must_use]
    pub const fn used_x10(&self) -> u16 {
        match self {
            Bound::Used { pct_x10, .. } => *pct_x10,
            Bound::Wall { .. } => 1000,
        }
    }

    /// When the peer read it: the reading's instant, or when it saw the wall.
    #[must_use]
    pub const fn read(&self) -> i64 {
        match self {
            Bound::Used { read, .. } | Bound::Wall { read, .. } => *read,
        }
    }
}

/// A peer's bound of an account's week and five-hour window ([`bounds_of`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Bounds {
    /// The week's (`7d`).
    pub week: Option<Bound>,
    /// The five-hour window's (`5h`).
    pub five: Option<Bound>,
}

/// The display bound one journal window admits at `now` (`me`: this Mac's own Commons uuids,
/// [`Seen::me`]), or none. Admitted only for the account's own (never a workspace's) week or
/// five-hour window, made by a Mac that is not this one (told by its raw uuid; a fact with no
/// uuid is not admitted), dated at most [`BOUND_AHEAD_S`] ahead (and said as of now at the
/// latest): a standing wall whose end is known and ahead, else a figure of use whose reset is
/// known and ahead; either at most a window and [`BOUND_RESET_SLACK_S`] from now (a reset read
/// to the hour and rolled a day on names no window now). An unread reset, an unknown reset and a
/// wall with no end admit nothing.
#[must_use]
pub fn bound_of(
    window: &WindowSeen,
    me: &std::collections::BTreeSet<String>,
    now: i64,
) -> Option<Bound> {
    let length = match window.window.as_str() {
        WEEK => super::accounts::WEEK_WINDOW_S,
        FIVE => super::accounts::SESSION_WINDOW_S,
        _ => return None,
    };
    if window.sub.is_some() {
        return None;
    }
    let peer = |uuid: &str| !me.contains(uuid);
    let dated = |t: i64| (t <= now.saturating_add(BOUND_AHEAD_S)).then_some(t.min(now));
    let latest = now
        .saturating_add(length)
        .saturating_add(BOUND_RESET_SLACK_S);
    let ahead = |end: i64| now < end && end <= latest;
    let wall = window.wall_by.as_ref().and_then(|(t, by, uuid)| {
        let until = window.wall_until.flatten().filter(|until| ahead(*until))?;
        peer(uuid).then_some(())?;
        Some(Bound::Wall {
            until,
            read: dated(*t)?,
            by: by.clone(),
        })
    });
    if wall.is_some() {
        return wall;
    }
    let Some(Figure::Used {
        pct_x10,
        reset: Some(reset),
    }) = window.figure
    else {
        return None;
    };
    let (t, by, src) = window.read.as_ref()?;
    peer(window.reader.as_deref()?).then_some(())?;
    ahead(reset).then_some(())?;
    Some(Bound::Used {
        pct_x10,
        reset,
        read: dated(*t)?,
        by: by.clone(),
        src: src.clone(),
    })
}

/// The display bounds of one account's week and five-hour window at `now` ([`bound_of`]).
#[must_use]
pub fn bounds_of(
    account: &AccountSeen,
    me: &std::collections::BTreeSet<String>,
    now: i64,
) -> Bounds {
    let of = |word: &str| {
        account
            .windows
            .iter()
            .filter(|w| w.window == word)
            .find_map(|w| bound_of(w, me, now))
    };
    Bounds {
        week: of(WEEK),
        five: of(FIVE),
    }
}

/// Each roster row's display bounds at `now` from the journal snapshot `seen`, by its label: a
/// row the journal admits nothing of is left out. A pure function of what the snapshot holds; the
/// outlook and the card show what it gives, and no decision reads it.
#[must_use]
pub fn bounds(
    cli: super::accounts::Cli,
    roster: &super::accounts::Roster,
    seen: &Seen,
    now: i64,
) -> BTreeMap<String, Bounds> {
    roster
        .rows
        .iter()
        .filter_map(|row| {
            let account = seen.accounts.get(&row_key(cli, row)?)?;
            let bounds = bounds_of(account, &seen.me, now);
            (bounds != Bounds::default()).then(|| (row.label.clone(), bounds))
        })
        .collect()
}

/// The current row's canonical claims key. Historical label state and registry alias matches
/// do not establish which account the row names now.
#[must_use]
pub fn row_key(cli: super::accounts::Cli, row: &super::accounts::Row) -> Option<String> {
    super::journal::account_key(cli, row).map(|key| key.as_str().to_owned())
}

/// One roster row the journal says something of: its label, its account key, and what it says.
pub type Reported = (String, String, AccountSeen);

/// Each row of `roster` the journal says something of, in roster order.
#[must_use]
pub fn rows(
    cli: super::accounts::Cli,
    roster: &super::accounts::Roster,
    seen: &Seen,
) -> Vec<Reported> {
    roster
        .rows
        .iter()
        .filter_map(|row| {
            let pk = row_key(cli, row)?;
            let found = seen.accounts.get(&pk)?;
            Some((row.label.clone(), pk, found.clone()))
        })
        .collect()
}

/// The report's `journal` section: per row, by its account's key (`ac`, the key the journal
/// holds it by) with its slug and name as the report names the row (`names`, each null where it
/// has none), its last use, sign-ins and windows, each time with its signed age at `now`, or
/// null if that age cannot fit in an i64. Only a row with a key is here ([`rows`]), so no roster
/// label is.
#[must_use]
pub fn rows_json(
    rows: &[Reported],
    names: &super::account_names::Listing,
    now: i64,
) -> aterm_json::Value {
    use aterm_json::{Map, Value, json};
    let mut out = Map::new();
    for (label, pk, seen) in rows {
        let account = names.names(label).map(|(_, names)| names);
        let windows: Vec<Value> = seen
            .windows
            .iter()
            .map(|w| {
                let w = w.as_of(now);
                let (state, pct, reset) = match &w.figure {
                    Some(Figure::Used { pct_x10, reset }) => (
                        if reset.is_some() {
                            "current"
                        } else {
                            "reset-unknown"
                        },
                        Some(*pct_x10),
                        *reset,
                    ),
                    Some(Figure::ResetUnread { reset }) => ("reset-unread", None, Some(*reset)),
                    None if w.wall_until.is_some() => ("wall-only", None, None),
                    None => ("unknown", None, None),
                };
                json!({
                    "window": w.window,
                    "model": w.model,
                    "sub": w.sub,
                    "state": state,
                    "used_pct_x10": pct,
                    "reset": reset,
                    "read": w.read.as_ref().map(|(t, by, src)| json!({
                        "t": t, "age_s": now.checked_sub(*t), "by": by, "src": src,
                    })),
                    "wall_until": w.wall_until.map(|u| json!(u)),
                    "line": window_line(&w, now),
                })
            })
            .collect();
        out.insert(
            pk.clone(),
            json!({
                "ac": pk,
                "slug": account.map(|names| names.slug.clone()).filter(|slug| !slug.is_empty()),
                "name": account.and_then(|names| names.name.clone()),
                "last_use": seen.last_use.as_ref().map(|l| json!({
                    "by": l.by, "t": l.t, "age_s": now.checked_sub(l.t), "clock": "its",
                })),
                "signed_in": seen.signed_in.iter().map(|s| json!({
                    "on": s.on, "state": s.state, "t": s.t,
                })).collect::<Vec<_>>(),
                "windows": windows,
            }),
        );
    }
    Value::Object(out)
}

/// The report's journal section as text, after the local report: each row as the report names
/// it (`names`), by its account's name and slug.
#[must_use]
pub fn rows_text(rows: &[Reported], names: &super::account_names::Listing, now: i64) -> String {
    let mut out = String::from("\nlab journal (what every Mac saw; no provider asked):\n");
    for (label, _, seen) in rows {
        let shown = names.shown(label);
        for line in lines(seen, now) {
            out.push_str(&format!("  {shown}: {line}\n"));
        }
    }
    out
}

/// What `accounts history` asks about: a row's label on this Mac and the account's key, either
/// or both. The label is the local key the ledger's rows hold; nothing prints it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Asked {
    pub label: Option<String>,
    pub pk: Option<String>,
}

/// The history of `asked`, oldest first, each line with its instant: this Mac's ledger rows about
/// it (by key, or by label for a row written before its key was learned), as `here`, and every
/// Mac's journal events about it. A ledger row holds its accounts by row label, the local key,
/// and `name` names each as the line prints it: the label the row holds, for the account the
/// row is about the account key it carried (`to=` carries none), and the row's instant, since a
/// label may have named another account then. A ledger row's free-text reason is never shown,
/// only its kind and its accounts' names.
#[must_use]
pub fn history(
    asked: &Asked,
    ledger: &[super::accounts::Event],
    journal: Option<&AccountSeen>,
    name: &dyn Fn(&str, Option<&str>, i64) -> String,
) -> Vec<(i64, String)> {
    let mut out: Vec<(i64, u8, String)> = Vec::new();
    for event in ledger {
        let mine = of_asked(asked, event)
            || asked.label.is_some() && event.to.as_deref() == asked.label.as_deref();
        if !mine {
            continue;
        }
        let mut line = format!(
            "{} here {} {}",
            super::usage::rfc3339_utc(event.t),
            event.kind.as_str(),
            name(&event.account, event.pk.as_deref(), event.t)
        );
        if let Some(to) = &event.to {
            line.push_str(&format!(" to={}", name(to, None, event.t)));
        }
        out.push((event.t, 0, line));
    }
    for line in journal
        .map(|seen| seen.history.as_slice())
        .unwrap_or_default()
    {
        out.push((line.t, 1, history_line(line)));
    }
    out.sort();
    out.into_iter().map(|(t, _, line)| (t, line)).collect()
}

/// Whether this Mac's ledger row `event` is about `asked`'s account: by key, or by label for a row
/// written before its key was learned.
fn of_asked(asked: &Asked, event: &super::accounts::Event) -> bool {
    match (&asked.pk, &event.pk) {
        (Some(pk), Some(key)) => pk == key,
        _ => asked.label.as_deref() == Some(event.account.as_str()),
    }
}

/// Each window a limit reset this Mac's aterm spent on `asked` refilled, with the reset's instant:
/// the windows each [`super::accounts::Kind::ResetUsed`] row about it names
/// ([`super::accounts::Event::refilled`]). A refill or raise a reading showed is no such row
/// ([`super::accounts::Kind::Reset`]).
#[must_use]
pub fn spent(asked: &Asked, ledger: &[super::accounts::Event]) -> Vec<(i64, String)> {
    ledger
        .iter()
        .filter(|event| event.kind == super::accounts::Kind::ResetUsed && of_asked(asked, event))
        .flat_map(|event| event.refilled.iter().map(|w| (event.t, w.clone())))
        .collect()
}

/// One history line as text: "2026-11-03T17:00:00Z m17 signed-in claude".
#[must_use]
pub fn history_line(line: &Line) -> String {
    let mut out = format!(
        "{} {} {} {}",
        super::usage::rfc3339_utc(line.t),
        line.by,
        line.kind,
        line.cli
    );
    if !line.detail.is_empty() {
        out.push(' ');
        out.push_str(&line.detail);
    }
    if line.heard {
        out.push_str(" (heard by this Mac)");
    }
    out
}

#[cfg(test)]
#[path = "alab_seen_tests.rs"]
mod tests;
