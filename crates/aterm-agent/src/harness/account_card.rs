// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What the Account card says of the one account a tab runs on, as typed facts: who the account
//! is, its stored login, its budget, its history on this Mac and which Macs hold it.
//!
//! [`build`] is pure. It reads no file, asks no provider and waits on nothing: the worker that
//! opens the card reads the roster, the accounts state, the pool file, the claims table and the
//! series first, and hands them over in [`Inputs`]. (The account's lab key is worked out through
//! the process's installed journal keyer, [`super::journal::claude_email_key`], which is memory
//! alone.) The card's words are the window's to choose; nothing here is a sentence.
//!
//! Three rules hold whatever the inputs:
//! - **An outside login is never given an end.** For an account no roster row lists
//!   ([`Who::Outside`]) nothing here knows when its stored login ends, so [`AccountCard::login`]
//!   is `None` for it and no date is made up.
//! - **"In use" is a live claim.** A Mac is listed as holding the account only while its claim
//!   in the claims table is live ([`super::claims::Book::live_claims`]), or, where this process
//!   reads no table, as the senior holder this Mac last settled ([`Claims::Senior`]). A claim
//!   outlives use (an hour or two, a day for an offline home Mac), so a holder carries when its
//!   claim began and never "running now".
//! - **Unknown is not none.** No pool file, no claims table, an unreadable table and a journal
//!   this process does not read are each their own value ([`Home::NoPoolFile`],
//!   [`Claims::Absent`], [`Claims::Unreadable`], [`JournalHeld::Off`]), never an empty list.
//!   An account nothing names by an address was not looked up at all, and says that
//!   ([`Home::NotAsked`], [`Claims::NotAsked`], [`HistoryFrom::NotRead`]): never "no home",
//!   "no other Mac" or "no history". A hold this Mac keeps for a Mac whose clock disagrees with
//!   its own is no claim that Mac made: it carries no beginning ([`Holder::since`] is `None`).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use super::account_history::{HistoryDay, HistoryRead, HistoryWho};
use super::accounts::{self, Cli, Logins, Roster, Row, State};
use super::ai_pool::{self, Pool, SlotWhy};
use super::alab_seen::{self, AccountSeen, Journal};
use super::claims::{self, LeaseKey};
use super::outlook::{self, Figure, Offsets};
use super::profile::AccountKey;

/// The account a tab's card is about.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Who {
    /// One of this Mac's roster rows (Claude Code).
    Pool {
        /// Its roster label: a key, never shown.
        label: String,
        /// The name it is shown by ([`Row::shown_name`]: its email).
        name: String,
        /// Whether a login was ever made for it on this Mac.
        enrolled: bool,
    },
    /// A Claude Code login no roster row lists: the tab runs on it, aterm cannot move it.
    Outside {
        /// The address the session's profile names.
        email: String,
    },
    /// Another pooled CLI's current account on this Mac (the store's active row): the footer of
    /// such a program names no login of its own.
    Active {
        /// Its roster label.
        label: String,
        /// The name it is shown by.
        name: String,
    },
    /// Nothing names an account: no row answers and the session names no address.
    Unknown,
}

impl Who {
    /// The roster label, where the account is a row here.
    #[must_use]
    pub fn label(&self) -> Option<&str> {
        match self {
            Who::Pool { label, .. } | Who::Active { label, .. } => Some(label),
            Who::Outside { .. } | Who::Unknown => None,
        }
    }
}

/// A Mac as the card names it.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Mac {
    /// Its lab label (`m15`); empty where it has none ([`Self::named`] is false).
    pub name: String,
    /// It is this Mac ([`State::owns_swarm_label`], or the claims table's own machine).
    pub ours: bool,
    /// It has a lab label. A holder known only as `claim:<machine>`, a journal `uuid:<hex>` or
    /// a row with an empty label has none.
    pub named: bool,
}

impl Mac {
    /// The Mac a record names by `raw`: an `m<N>` label, this Mac's own machine id, or one of
    /// the unnamed forms.
    fn of(state: &State, raw: &str) -> Mac {
        let ours = state.owns_swarm_label(raw);
        let label = ai_pool::leading_m_label(raw.trim_ascii())
            .map(|(_, canon)| canon)
            .or_else(|| state.swarm_label.clone().filter(|_| ours));
        Mac {
            named: label.is_some(),
            name: label.unwrap_or_default(),
            ours,
        }
    }
}

/// One window's figure as it was read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Seen {
    /// What is left, under the glass's age rule ([`Figure::of_reading`]); [`Figure::Out`] for a
    /// standing wall. Never [`Figure::Unread`]: an unread window has no `Seen`.
    pub figure: Figure,
    /// The window's reset, where the reading or the wall named one.
    pub reset: Option<i64>,
    /// When it was read, where that is known.
    pub at: Option<i64>,
    /// The Mac that read it; `None` for this Mac.
    pub by: Option<Mac>,
}

/// Whether usage credits carry the account.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CreditsWord {
    /// On, as a limit row said.
    On,
    /// Seen carrying its sessions past a limit, since this instant.
    InUse {
        /// The first answer seen served on credits, this episode.
        since: i64,
    },
    /// Off.
    Off,
    /// Out of credits (a profile's cache says so; the accounts state never does).
    Out,
}

impl CreditsWord {
    /// The word of a profile's cache ([`super::profile::Credits::state`]: `on`, `out`, `off`).
    #[must_use]
    pub fn of_state(word: &str) -> Option<CreditsWord> {
        match word {
            "on" => Some(CreditsWord::On),
            "out" => Some(CreditsWord::Out),
            "off" => Some(CreditsWord::Off),
            _ => None,
        }
    }
}

/// The account's budget as this Mac's records and the journal hold it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Budget {
    /// The week of all models.
    pub week: Option<Seen>,
    /// The five-hour window.
    pub five: Option<Seen>,
    /// Each per-model week that holds a figure or a wall, by the model's name (`Fable`).
    pub models: Vec<(String, Seen)>,
    /// Usage credits, where they were observed.
    pub credits: Option<CreditsWord>,
    /// Each standing wall no figure above already says: its window in words (`month`) and the
    /// reset it named, where that is still ahead.
    pub walls: Vec<(String, Option<i64>)>,
    /// A limit-reset offer that still stands.
    pub resets: Option<accounts::Offer>,
}

/// Where the card's history comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HistoryFrom {
    /// This Mac's readings series holds rows of the account.
    Series,
    /// This Mac's series holds no row of the account, and the journal holds its week as the
    /// lab's Macs read it ([`alab_seen::week_reads`]).
    Journal,
    /// Looked for, by label and by key, and nothing found.
    Nothing,
    /// Not looked for: nothing names the account, or this Mac's accounts could not be read, so
    /// no series was asked.
    NotRead,
    /// No series is kept for this program's accounts (a pooled CLI other than Claude Code).
    NotKept,
}

/// One local day of the account's week.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DayPoint {
    /// The day's local midnight.
    pub midnight: i64,
    /// What was left of the week at the day's last reading, in tenths; `None` where the day
    /// holds no weekly reading.
    pub left_x10: Option<u16>,
    /// The day's last weekly reading was 100% used.
    pub out: bool,
    /// A five-hour window read full that day.
    pub five_walled: bool,
}

/// The account's history: this Mac's own readings, or the lab's where this Mac holds none.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct History {
    /// Where it comes from.
    pub from: HistoryFrom,
    /// The week as read over the last eight days: the last reading of each hour, oldest first
    /// ([`outlook::reads_of`], or the journal's through [`outlook::hourly`]).
    pub reads: Vec<outlook::Read>,
    /// The local days inside the span read that hold anything, oldest first. A day missing
    /// from the list was not read. The journal types no day rows, so a history from it has
    /// none.
    pub days: Vec<DayPoint>,
    /// The oldest row this Mac holds of the account, whatever the span; `None` for a history
    /// from the journal. With [`HistoryFrom::Nothing`] it says this Mac holds rows of the
    /// account and none of them is a reading of its week inside the span.
    pub since: Option<i64>,
    /// The series files' stamp the history was folded at
    /// ([`super::account_history::stamp`]): a caller keeps it while this holds.
    pub stamp: u64,
}

impl History {
    /// A history that holds nothing, for the reason `from` gives.
    #[must_use]
    pub fn none(from: HistoryFrom) -> History {
        History {
            from,
            reads: Vec::new(),
            days: Vec::new(),
            since: None,
            stamp: 0,
        }
    }

    /// The history a series read gives at `now`: its eight days of readings and its days.
    /// [`HistoryFrom::Nothing`] where the read found no reading of the account's week: no row
    /// at all, or rows that hold none inside the span (its plan alone, or readings older than
    /// the span), which then keep their `since`. Such a series has nothing to draw and no
    /// week to date, and the lab's readings stand in for it as for no series.
    #[must_use]
    pub fn of_read(read: &HistoryRead, stamp: u64, now: i64) -> History {
        let reads = outlook::reads_of(&read.rows, now);
        let days: Vec<DayPoint> = read.days.iter().map(day_point).collect();
        if reads.is_empty() && days.iter().all(|day| day.left_x10.is_none()) {
            return History {
                since: read.oldest,
                stamp,
                ..History::none(HistoryFrom::Nothing)
            };
        }
        History {
            from: HistoryFrom::Series,
            reads,
            days,
            since: read.oldest,
            stamp,
        }
    }
}

/// `own`, this Mac's history, or the journal's week where this Mac holds no row of the account
/// and `seen` holds readings of the last eight days. It keeps `own`'s stamp, so a caller that
/// keeps the result asks the journal again at each build and never the files.
fn or_the_journals(own: Arc<History>, seen: Option<&AccountSeen>, now: i64) -> Arc<History> {
    if matches!(own.from, HistoryFrom::Series | HistoryFrom::NotKept) {
        return own;
    }
    let reads = outlook::hourly(
        seen.map(alab_seen::week_reads)
            .unwrap_or_default()
            .into_iter()
            .map(|(t, x10, reset)| outlook::Read {
                t,
                x10: x10.min(1000),
                reset,
                epoch: None,
            })
            .collect(),
        now,
    );
    let from = match (reads.is_empty(), own.from) {
        (false, _) => HistoryFrom::Journal,
        // No series was asked, and the journal holds nothing: still not read, never none.
        (true, HistoryFrom::NotRead) => HistoryFrom::NotRead,
        (true, _) => HistoryFrom::Nothing,
    };
    if own.from == from && own.reads == reads {
        return own;
    }
    Arc::new(History {
        from,
        reads,
        stamp: own.stamp,
        ..History::none(from)
    })
}

fn day_point(day: &HistoryDay) -> DayPoint {
    let sum = day.sum.as_ref();
    let week = sum.and_then(|s| s.seven_day);
    DayPoint {
        midnight: day.t,
        left_x10: week.map(|w| u16::try_from(1000_u32.saturating_sub(w.last)).unwrap_or(0)),
        out: week.is_some_and(|w| w.last >= 1000),
        five_walled: sum
            .and_then(|s| s.five_hour.as_ref())
            .is_some_and(|five| five.walled > 0),
    }
}

/// The series read a card is built from.
#[derive(Clone, Debug)]
pub enum HistoryIn<'a> {
    /// A read just made, and the stamp it was made at.
    Read {
        /// What [`super::account_history::read`] found.
        read: &'a HistoryRead,
        /// [`super::account_history::stamp`] then.
        stamp: u64,
    },
    /// The history of an earlier build of the same account, kept while its stamp and the local
    /// day held. One that is not this Mac's own readings is read from the journal again.
    Kept(Arc<History>),
    /// No read was made.
    NotRead,
}

/// Where the account's home Mac is on record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Home {
    /// The pool file names a home Mac for it.
    Known {
        /// Its home (`warm[0]`).
        mac: Mac,
        /// Why it holds the slot, where the file says.
        why: Option<SlotWhy>,
        /// Since when, where the file's word reads as a date or an instant. A date alone is
        /// its noon UTC, so the local date said is the file's.
        since: Option<i64>,
        /// The other Macs it is assigned to, in the file's order.
        also: Vec<Mac>,
    },
    /// This Mac has no pool file, or it could not be read.
    NoPoolFile,
    /// The pool file lists no such account, or lists it with no Mac.
    NotListed,
    /// Nothing names the account by an address, so no file was asked for its home.
    NotAsked,
}

/// One Mac's live claim on the account, or the hold this Mac keeps for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Holder {
    /// The Mac.
    pub mac: Mac,
    /// When its uninterrupted claim began. `None` for a hold this Mac keeps on the account for
    /// its home Mac while that Mac's clock disagrees with this one's ([`claims::holds`]): its
    /// claims cannot be seen, so whether it uses the account is not known and nothing began.
    pub since: Option<i64>,
    /// When its claim (or the hold) lapses unless renewed.
    pub until: i64,
}

/// Why no claims table answered for the Macs beside a senior holder ([`Claims::Senior`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoTable {
    /// This process reads none.
    Absent,
    /// It could not be read just now.
    Unreadable,
    /// Nothing names the account by an address the table is keyed by.
    NotAsked,
}

/// Which Macs hold the account now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Claims {
    /// The claims table was read: every Mac with a live claim, the earliest claim first, then
    /// each Mac this Mac only holds the account for. Empty means no Mac claims it.
    Read(Vec<Holder>),
    /// No table spoke of the account, for the reason given, but this Mac's accounts state holds
    /// the senior claim it last settled, still live: one holder is known, the others are not.
    Senior(Holder, NoTable),
    /// This process reads no claims table (a debug build, a lab instance), or claims do not
    /// apply here (no pool file) and the table names nobody.
    Absent,
    /// The table could not be read just now.
    Unreadable,
    /// Claims are not kept for this program's accounts ([`claims::CLAIMED_CLIS`]).
    NotTracked,
    /// Nothing names the account by an address the table is keyed by, so it was not asked.
    NotAsked,
}

/// What the journal holds of the account.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JournalHeld {
    /// This process reads no journal.
    Off,
    /// It reads one and holds nothing yet.
    NotReadYet,
    /// It was read and says nothing of this account (or the account has no lab key here).
    Silent,
    /// It was read and says something of this account.
    Held,
}

/// Where the account lives and who uses it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Machine {
    /// Its home.
    pub home: Home,
    /// Who holds it now.
    pub claims: Claims,
    /// The journal's last use: the Mac and when, by that Mac's clock.
    pub last_use: Option<(Mac, i64)>,
    /// The pool file's last-used day of the home slot (its noon UTC).
    pub used_day: Option<i64>,
    /// Sessions started on it by Mac, most first, and the instant the count starts from
    /// ([`alab_seen::starts_by_mac`]).
    pub starts: Option<(Vec<(Mac, u32)>, i64)>,
    /// What the journal held when this was built.
    pub journal: JournalHeld,
}

/// How near its end a stored login reads as `ending` (§5.9: about a week).
pub const ENDING_S: i64 = 7 * 86_400;

/// The login half of the offer's pair (§5.9): the account's stored login on this Mac. It lives
/// here, beside the card that shows it, because the token view is Unix-only and the card is not.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoginHalf {
    /// A token is in use: it carries the sessions past the login's end (`-`).
    InUse,
    /// [`ENDING_S`] or more left, or no end known.
    Ok,
    /// Under [`ENDING_S`] left, ending at `end` (unix seconds).
    Ending {
        /// The forecast end.
        end: i64,
    },
    /// Ended, at `at` where it is known.
    Ended {
        /// When.
        at: Option<i64>,
    },
}

impl LoginHalf {
    /// The half of `row`'s stored login here at `now`, from its records
    /// ([`accounts::signed_out_here`], [`accounts::login_forecast`]).
    #[must_use]
    pub fn of_row(row: &Row, logins: &Logins, state: &State, now: i64) -> Self {
        if accounts::signed_out_here(row, state) {
            return Self::Ended {
                at: accounts::login_ended_at(row, logins, state),
            };
        }
        match accounts::login_forecast(row, logins, state) {
            Some(end) if end <= now => Self::Ended { at: Some(end) },
            Some(end) if end - now < ENDING_S => Self::Ending { end },
            _ => Self::Ok,
        }
    }

    /// The half's word in the pair.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::InUse => "-",
            Self::Ok => "ok",
            Self::Ending { .. } => "ending",
            Self::Ended { .. } => "ended",
        }
    }

    /// When the half began, where a date says it.
    #[must_use]
    pub const fn since(self) -> Option<i64> {
        match self {
            Self::Ending { end } => Some(end - ENDING_S),
            Self::Ended { at } => at,
            Self::InUse | Self::Ok => None,
        }
    }
}

/// Everything the Account card says of one account that a file, a table or the journal holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountCard {
    /// The account.
    pub who: Who,
    /// Each roster row's shown name, by label: what a label in a record is shown as.
    pub names: BTreeMap<String, String>,
    /// How many accounts this Mac's roster lists.
    pub pool_size: usize,
    /// This Mac's lab label, where it has one.
    pub this_mac: Option<String>,
    /// The account's stored login here: a roster row's whose login was made on this Mac. `None`
    /// for a row never signed in here, and for an outside login, whose end nothing here knows.
    pub login: Option<LoginHalf>,
    /// Its budget.
    pub budget: Budget,
    /// Its history on this Mac.
    pub history: Arc<History>,
    /// Its Macs.
    pub machine: Machine,
    /// The shown name of the account this Mac is pinned to, where it is pinned.
    pub pinned: Option<String>,
    /// The local clock the card's times are said on, sampled over every instant a card says
    /// ([`CLOCK_BACK_S`] back, [`CLOCK_AHEAD_S`] ahead): each is said on its own offset, across
    /// a daylight-saving change.
    pub offsets: Offsets,
    /// When it was built.
    pub read_at: i64,
}

/// The session a card is opened from, as its footer names it.
#[derive(Clone, Copy, Debug, Default)]
pub struct Session<'a> {
    /// The roster label its login is judged to (`SubscriptionDisplay::label`), else the account
    /// its footer names.
    pub label: Option<&'a str>,
    /// The address its profile names.
    pub email: Option<&'a str>,
    /// The account key its profile names.
    pub profile_key: Option<AccountKey>,
}

/// What the worker read, for [`build`].
#[derive(Clone, Debug)]
pub struct Inputs<'a> {
    /// The pooled CLI the tab runs.
    pub cli: Cli,
    /// Its roster.
    pub roster: &'a Roster,
    /// Its accounts state.
    pub state: &'a State,
    /// The rows whose login is made on this Mac.
    pub enrolled: &'a BTreeSet<String>,
    /// The login records.
    pub logins: &'a Logins,
    /// The session.
    pub session: Session<'a>,
    /// The pool file, where one was read.
    pub pool: Option<&'a Pool>,
    /// The claims table's read ([`claims::read_book`]).
    pub claims: &'a Result<Option<(claims::View, claims::Book)>, String>,
    /// The journal ([`alab_seen::journal`]).
    pub journal: &'a Journal,
    /// The series read.
    pub history: HistoryIn<'a>,
    /// Unix seconds.
    pub now: i64,
    /// The local clock: an instant's offset from UTC, seconds east.
    pub tz_at: fn(i64) -> i64,
}

/// The roster row a session runs on: the row `label` names, unless its expected address
/// contradicts the session's `email`; with no label, the row whose expected address is the
/// session's. Never the store's active fallback: a session's own figures belong to its own
/// account alone.
#[must_use]
pub fn session_row<'a>(
    roster: &'a Roster,
    label: Option<&str>,
    email: Option<&str>,
) -> Option<&'a Row> {
    match label {
        Some(label) => roster.row(label).filter(|row| {
            email.is_none_or(|email| {
                row.expected_identity_email()
                    .is_none_or(|expected| accounts::email_matches(expected, email))
            })
        }),
        None => email.and_then(|email| {
            roster.rows.iter().find(|row| {
                row.expected_identity_email()
                    .is_some_and(|expected| accounts::email_matches(expected, email))
            })
        }),
    }
}

/// Which account the card is about ([`Who`]): for Claude Code the session's own row
/// ([`session_row`]), else the address its profile names; for another pooled CLI the store's
/// active row.
#[must_use]
pub fn who_of(
    cli: Cli,
    roster: &Roster,
    state: &State,
    enrolled: &BTreeSet<String>,
    session: &Session<'_>,
) -> Who {
    if cli != Cli::Claude {
        return state
            .active
            .as_deref()
            .and_then(|label| roster.row(label))
            .map_or(Who::Unknown, |row| Who::Active {
                label: row.label.clone(),
                name: row.shown_name().to_owned(),
            });
    }
    match session_row(roster, session.label, session.email) {
        Some(row) => Who::Pool {
            label: row.label.clone(),
            name: row.shown_name().to_owned(),
            enrolled: enrolled.contains(&row.label),
        },
        None => session
            .email
            .filter(|email| !email.is_empty())
            .map_or(Who::Unknown, |email| Who::Outside {
                email: email.to_owned(),
            }),
    }
}

/// The account's lab key (`ac-…`) as the journal keys it: a row's recorded key, else the one
/// its row gives now; an outside login's by its address. `None` where this process keys nothing
/// (no journal sink).
fn lab_key(cli: Cli, roster: &Roster, state: &State, who: &Who) -> Option<String> {
    match who {
        Who::Pool { label, .. } | Who::Active { label, .. } => state
            .accounts
            .get(label)
            .and_then(|account| account.pk.clone())
            .or_else(|| alab_seen::row_key(cli, roster.row(label)?)),
        Who::Outside { email } => {
            super::journal::claude_email_key(email).map(|key| key.as_str().to_owned())
        }
        Who::Unknown => None,
    }
}

/// Whose series rows a card's history is ([`super::account_history::read`]): a row's label
/// with its record's keys, an outside login by its lab key and the account key its profile
/// names. `None` where no series is kept (another CLI) or nothing names the account.
#[must_use]
pub fn history_who(
    cli: Cli,
    roster: &Roster,
    state: &State,
    session: &Session<'_>,
    who: &Who,
) -> Option<HistoryWho> {
    if cli != Cli::Claude {
        return None;
    }
    let pk = lab_key(cli, roster, state, who);
    match who {
        Who::Pool { label, .. } => Some(HistoryWho {
            label: Some(label.clone()),
            pk,
            key: state.accounts.get(label).and_then(|account| account.key),
        }),
        Who::Outside { .. } => Some(HistoryWho {
            label: None,
            pk,
            key: session.profile_key,
        }),
        Who::Active { .. } | Who::Unknown => None,
    }
}

/// A stored figure as a [`Seen`] at `now`; `None` where it is unread by then.
fn seen_of_fig(state: &State, fig: &accounts::WindowFig, now: i64) -> Option<Seen> {
    let figure = Figure::of_reading(fig.pct_x10, fig.reset, fig.at, now);
    (figure != Figure::Unread).then(|| Seen {
        figure,
        reset: Some(fig.reset),
        at: Some(fig.at),
        by: Some(Mac::of(state, &fig.by)).filter(|mac| !mac.ours),
    })
}

/// A standing wall as a [`Seen`]: out, until the reset it named where that is ahead.
fn seen_of_wall(state: &State, wall: &accounts::Wall, now: i64) -> Option<Seen> {
    wall.stands_until()
        .is_some_and(|end| now < end)
        .then(|| Seen {
            figure: Figure::Out,
            reset: wall.until.filter(|until| *until > now),
            at: Some(wall.at),
            by: Some(Mac::of(state, &wall.by)).filter(|mac| !mac.ours),
        })
}

/// One of the journal's windows as a [`Seen`] at `now`: a standing wall, else its figure where
/// its reset is known and ahead and its reading's clock admits it.
fn seen_of_journal(state: &State, window: &alab_seen::WindowSeen, now: i64) -> Option<Seen> {
    let window = window.as_of(now);
    let (at, by) = match &window.read {
        Some((t, mac, _)) => (Some(*t), Some(Mac::of(state, mac)).filter(|mac| !mac.ours)),
        None => (None, None),
    };
    if let Some(until) = window.wall_until {
        return Some(Seen {
            figure: Figure::Out,
            reset: until,
            at,
            by,
        });
    }
    match window.figure? {
        alab_seen::Figure::Used {
            pct_x10,
            reset: Some(reset),
        } => {
            let figure = Figure::of_reading(u32::from(pct_x10), reset, at?, now);
            (figure != Figure::Unread).then_some(Seen {
                figure,
                reset: Some(reset),
                at,
                by,
            })
        }
        alab_seen::Figure::Used { reset: None, .. } | alab_seen::Figure::ResetUnread { .. } => None,
    }
}

/// The journal's own window `word` (`5h`, `7d`) of the account, never a workspace's.
fn journal_window(
    state: &State,
    journal: Option<&AccountSeen>,
    word: &str,
    now: i64,
) -> Option<Seen> {
    journal?
        .windows
        .iter()
        .find(|w| w.window == word && w.sub.is_none())
        .and_then(|w| seen_of_journal(state, w, now))
}

/// A per-model week's name: the one the stats view gives (`Fable`), else its bucket's.
fn model_name(win: &super::account_stats::Win) -> Option<String> {
    use super::profile::Bucket;
    win.name.clone().or_else(|| {
        [
            (Bucket::Fable, "Fable"),
            (Bucket::SevenDaySonnet, "Sonnet"),
            (Bucket::SevenDayOpus, "Opus"),
        ]
        .into_iter()
        .find(|(bucket, _)| bucket.id() == win.id)
        .map(|(_, name)| name.to_owned())
    })
}

/// A wall's window in words, for one no figure row says.
fn wall_window(id: &str) -> String {
    use super::profile::Bucket;
    match id {
        accounts::SESSION_LIMIT => "5-hour".to_owned(),
        accounts::WEEK_LIMIT => "week".to_owned(),
        accounts::MONTH_LIMIT => "month".to_owned(),
        id if id == Bucket::Fable.id() => "Fable week".to_owned(),
        id if id == Bucket::SevenDaySonnet.id() => "Sonnet week".to_owned(),
        id if id == Bucket::SevenDayOpus.id() => "Opus week".to_owned(),
        _ => "another".to_owned(),
    }
}

fn budget_of(
    cli: Cli,
    state: &State,
    who: &Who,
    journal: Option<&AccountSeen>,
    now: i64,
) -> Budget {
    let account = who.label().and_then(|label| state.accounts.get(label));
    let mut budget = Budget {
        five: journal_window(state, journal, "5h", now),
        week: journal_window(state, journal, "7d", now),
        ..Budget::default()
    };
    let Some(account) = account else {
        return budget;
    };
    // What this Mac's own records hold comes before the journal's word.
    let mut said: BTreeSet<&str> = BTreeSet::new();
    let stored = |id: &'static str| -> Option<Seen> {
        if let Some(wall) = account
            .walls
            .get(id)
            .and_then(|w| seen_of_wall(state, w, now))
        {
            return Some(wall);
        }
        let fig = if cli == Cli::Claude {
            let view = super::account_stats::held_windows(
                account,
                account.key,
                Some(&state.machine),
                super::account_stats::Tap::On,
                now,
            );
            view.windows
                .iter()
                .find(|w| w.id == id)
                .and_then(|w| w.current(now).cloned())
        } else {
            account.windows.get(id).filter(|fig| !fig.cut()).cloned()
        };
        fig.and_then(|fig| seen_of_fig(state, &fig, now))
    };
    for (id, slot) in [
        (accounts::SESSION_LIMIT, &mut budget.five),
        (accounts::WEEK_LIMIT, &mut budget.week),
    ] {
        said.insert(id);
        if let Some(seen) = stored(id) {
            *slot = Some(seen);
        }
    }
    if cli == Cli::Claude {
        for win in
            super::account_stats::held_model_weeks(account, account.key, Some(&state.machine), now)
        {
            let Some(name) = model_name(&win) else {
                continue;
            };
            let seen = account
                .walls
                .get(win.id)
                .and_then(|w| seen_of_wall(state, w, now))
                .or_else(|| {
                    win.current(now)
                        .and_then(|fig| seen_of_fig(state, fig, now))
                });
            if let Some(seen) = seen {
                said.insert(win.id);
                budget.models.push((name, seen));
            }
        }
    }
    budget.credits = account.credits.as_ref().map(|credits| match credits {
        accounts::Credits::InUse(episode) if episode.stands(now) => CreditsWord::InUse {
            since: episode.since,
        },
        accounts::Credits::InUse(_) | accounts::Credits::On(_) => CreditsWord::On,
        accounts::Credits::Off(_) => CreditsWord::Off,
    });
    budget.walls = account
        .walls
        .iter()
        .filter(|(id, wall)| {
            !said.contains(id.as_str()) && wall.stands_until().is_some_and(|end| now < end)
        })
        .map(|(id, wall)| (wall_window(id), wall.until.filter(|until| *until > now)))
        .collect();
    budget.resets = account
        .offer
        .clone()
        .filter(|offer| offer.left > 0 && offer.use_by.is_none_or(|by| by > now));
    budget
}

/// A pool file's date word as an instant: an RFC 3339 UTC instant as it is, a date alone as
/// its noon UTC (so the local date said is the file's own in every zone within twelve hours of
/// UTC).
fn pool_date(word: &str) -> Option<i64> {
    let word = word.trim_ascii();
    aterm_types::rfc3339::parse_utc_fractional(word).or_else(|| {
        (word.len() == 10)
            .then(|| aterm_types::rfc3339::parse_utc(&format!("{word}T12:00:00Z")))
            .flatten()
    })
}

/// The addresses the account goes by: the session's, or its row's login and expected identity.
fn addresses<'a>(roster: &'a Roster, who: &'a Who) -> Vec<&'a str> {
    match who {
        Who::Outside { email } => vec![email.as_str()],
        Who::Pool { label, .. } | Who::Active { label, .. } => {
            let row = roster.row(label);
            let mut out: Vec<&str> = row
                .and_then(Row::expected_identity_email)
                .into_iter()
                .chain(row.and_then(|row| row.email.as_deref()))
                .collect();
            out.dedup_by(|a, b| accounts::email_matches(a, b));
            out
        }
        Who::Unknown => Vec::new(),
    }
}

fn home_of(inputs: &Inputs<'_>, who: &Who) -> (Home, Option<i64>) {
    let addresses = addresses(inputs.roster, who);
    if addresses.is_empty() {
        return (Home::NotAsked, None);
    }
    let Some(pool) = inputs.pool else {
        return (Home::NoPoolFile, None);
    };
    let listed = addresses
        .into_iter()
        .find_map(|email| ai_pool::home_of_email(pool, inputs.cli, email));
    let Some((account, home)) = listed.and_then(|account| Some((account, account.home_mac()?)))
    else {
        return (Home::NotListed, None);
    };
    (
        Home::Known {
            mac: Mac::of(inputs.state, home),
            why: account.why.first().copied(),
            since: account.since.first().and_then(|word| pool_date(word)),
            also: account
                .warm
                .iter()
                .skip(1)
                .map(|slot| Mac::of(inputs.state, slot))
                .collect(),
        },
        account.used.first().and_then(|word| pool_date(word)),
    )
}

fn claims_of(inputs: &Inputs<'_>, who: &Who) -> Claims {
    if !claims::CLAIMED_CLIS.contains(&inputs.cli) {
        return Claims::NotTracked;
    }
    let state = inputs.state;
    let region = who
        .label()
        .and_then(|label| inputs.roster.row(label))
        .and_then(|row| row.vendor.region);
    let keys: Vec<LeaseKey> = addresses(inputs.roster, who)
        .into_iter()
        .filter_map(|email| LeaseKey::for_cli(inputs.cli, region, email).ok())
        .collect();
    // The senior holder this Mac last settled, where no table speaks.
    let senior = || {
        let lease = state
            .leases
            .get(who.label()?)
            .filter(|lease| lease.until > inputs.now)?;
        Some(Holder {
            mac: Mac::of(state, &lease.holder),
            since: Some(lease.at),
            until: lease.until,
        })
    };
    let unseen =
        |why: NoTable, none: Claims| senior().map_or(none, |held| Claims::Senior(held, why));
    if keys.is_empty() {
        // No address the table is keyed by: it was never asked, and an empty answer is none.
        return unseen(NoTable::NotAsked, Claims::NotAsked);
    }
    match inputs.claims {
        Ok(Some((view, book))) => {
            let mut holders: BTreeMap<String, Holder> = BTreeMap::new();
            for key in &keys {
                for held in book.live_claims(key, view, inputs.now) {
                    let mut mac = Mac::of(state, &held.label);
                    mac.ours |= held.machine == view.me;
                    if mac.ours && !mac.named {
                        mac.named = state.swarm_label.is_some();
                        mac.name = state.swarm_label.clone().unwrap_or_default();
                    }
                    let holder = Holder {
                        mac,
                        // A hold for a skewed peer is dated before every claim so that it is
                        // senior; that is no instant its claim began.
                        since: Some(held.since).filter(|since| *since != i64::MIN),
                        until: held.until,
                    };
                    holders
                        .entry(held.machine)
                        .and_modify(|kept| {
                            // The Mac's own live row says when its claim began; a hold alone
                            // says nothing of it.
                            kept.since = match (kept.since, holder.since) {
                                (Some(a), Some(b)) => Some(a.min(b)),
                                (a, b) => a.or(b),
                            };
                            kept.until = kept.until.max(holder.until);
                        })
                        .or_insert(holder);
                }
            }
            let mut holders: Vec<Holder> = holders.into_values().collect();
            holders.sort_by(|a, b| {
                (a.since.is_none(), a.since, &a.mac).cmp(&(b.since.is_none(), b.since, &b.mac))
            });
            if holders.is_empty() && inputs.pool.is_none() {
                // Claims are made only where a pool file is: an empty table proves nothing here.
                return unseen(NoTable::Absent, Claims::Absent);
            }
            Claims::Read(holders)
        }
        Ok(None) => unseen(NoTable::Absent, Claims::Absent),
        Err(_) => unseen(NoTable::Unreadable, Claims::Unreadable),
    }
}

/// How far back of now a card says an instant on its own offset: the day rows the series keeps
/// (400 days), which bound the `since` of a history, a home and a token's making.
pub const CLOCK_BACK_S: i64 = 400 * 86_400;

/// How far ahead of now a card says an instant on its own offset: a vault token's year, a
/// month's limit and a reset offer's last day all end inside it.
pub const CLOCK_AHEAD_S: i64 = 400 * 86_400;

/// The card of the account `inputs` names, from what the worker read. It reads no file.
#[must_use]
pub fn build(inputs: &Inputs<'_>) -> AccountCard {
    let (roster, state, now) = (inputs.roster, inputs.state, inputs.now);
    let who = who_of(inputs.cli, roster, state, inputs.enrolled, &inputs.session);
    let key = lab_key(inputs.cli, roster, state, &who);
    let (seen, journal) = match inputs.journal {
        Journal::Off => (None, JournalHeld::Off),
        Journal::NotReadYet => (None, JournalHeld::NotReadYet),
        Journal::Read(read) => match key.as_deref().and_then(|key| read.accounts.get(key)) {
            Some(account) => (Some(account), JournalHeld::Held),
            None => (None, JournalHeld::Silent),
        },
    };
    let login = who
        .label()
        .filter(|label| inputs.enrolled.contains(*label))
        .and_then(|label| roster.row(label))
        .map(|row| LoginHalf::of_row(row, inputs.logins, state, now));
    let history = match (&inputs.history, inputs.cli) {
        (_, cli) if cli != Cli::Claude => Arc::new(History::none(HistoryFrom::NotKept)),
        (HistoryIn::Kept(kept), _) => or_the_journals(Arc::clone(kept), seen, now),
        (HistoryIn::Read { read, stamp }, _) => {
            or_the_journals(Arc::new(History::of_read(read, *stamp, now)), seen, now)
        }
        (HistoryIn::NotRead, _) => {
            or_the_journals(Arc::new(History::none(HistoryFrom::NotRead)), seen, now)
        }
    };
    let (home, used_day) = home_of(inputs, &who);
    let machine = Machine {
        home,
        claims: claims_of(inputs, &who),
        last_use: seen
            .and_then(|account| account.last_use.as_ref())
            .map(|last| (Mac::of(state, &last.by), last.t)),
        used_day,
        starts: seen
            .and_then(alab_seen::starts_by_mac)
            .map(|(starts, since)| {
                (
                    starts
                        .into_iter()
                        .map(|(mac, n)| (Mac::of(state, &mac), n))
                        .collect(),
                    since,
                )
            }),
        journal,
    };
    AccountCard {
        budget: budget_of(inputs.cli, state, &who, seen, now),
        who,
        names: roster
            .rows
            .iter()
            .map(|row| (row.label.clone(), row.shown_name().to_owned()))
            .collect(),
        pool_size: roster.rows.len(),
        this_mac: state.swarm_label.clone(),
        login,
        history,
        machine,
        pinned: state
            .active
            .as_deref()
            .filter(|_| state.pinned)
            .map(|label| roster.row(label).map_or(label, Row::shown_name).to_owned()),
        offsets: Offsets::spanning(now - CLOCK_BACK_S, now + CLOCK_AHEAD_S, inputs.tz_at),
        read_at: now,
    }
}

#[cfg(test)]
#[path = "account_card_tests.rs"]
mod tests;
