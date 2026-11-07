// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The accounts outlook (`docs/DESIGN-accounts-outlook-2026-10-25.md`): what the rotation's own
//! policy would do if today's pace holds, from readings aterm already has. It moves nothing,
//! reads no account and probes nothing.
//!
//! [`outlook`] builds one [`Outlook`] from the roster, a copy of the accounts state and the
//! readings series ([`super::account_series`]):
//!
//! - **pace** is the work's, not an account's: the last 24 h of rises on every account, weighted
//!   by plan multiple ([`Demand`]), refused where its basis is too thin, too old or cut;
//! - **every row is filled in** with one freshness class per window ([`Fresh`]), and an unread
//!   figure is never a number;
//! - **the replay** drives the executor's own functions (`advise_past_probe`,
//!   `note_reading_held`, `note_wall`, `begin_rotation`, `prove`) on a clone of the state, over a
//!   7-day pass at the week's pace and a 12-hour working pass with the 5-hour windows, forking
//!   once at an account it cannot read ([`Lean`]). A stop open at a pass's horizon is followed
//!   past it, to a week beyond at most, to the instant the policy really resumes the work
//!   ([`StopEnd`]); each pass keeps every row's fill as it goes ([`Pass::fills`]).
//!
//! `docs/DESIGN-harness-glass-rainbow-2026-11-05.md` §3 supersedes the earlier design's stop
//! ends and time words: a stop never ends at the horizon or at a wall the worst case invented,
//! and a forecast instant is said to a grain its lead time can bear.
//!
//! The renderers ([`Outlook::glance`], [`Outlook::card_rows`], [`Outlook::text`],
//! [`Outlook::json`]) paint the one model, so the footer, the card and the CLI tell one story.
//!
//! The outlook is a leaf: nothing in the supervisor, the advice or the tap imports it; it has no
//! "next" or "best" accessor; it raises no message, entry, sound or `NEEDS YOU`.

use std::collections::{BTreeMap, BTreeSet};

use aterm_json::{Map, Value};

use super::account_series::{self, Began, PlanSaid, Run, SeriesRow};
use super::accounts::{
    self, Advice, Class, Condition, Me, Mode, Mover, Point, Roster, State, StayWhy, TapRead,
    WindowRead,
};

/// The week's pace looks back this far ([`Demand::week`]).
pub const WEEK_LOOKBACK_S: i64 = 24 * 3600;
/// The least span of readings the week's pace stands on.
pub const WEEK_LEAST_S: i64 = 2 * 3600;
/// The carrier's own week stands in for the pace once its window is this old.
pub const WEEK_FALLBACK_AGE_S: i64 = 12 * 3600;
/// The working pace looks back this far ([`Demand::five`]).
pub const FIVE_LOOKBACK_S: i64 = 3600;
/// The least span of readings the working pace stands on.
pub const FIVE_LEAST_S: i64 = 15 * 60;
/// The carrier's own 5-hour window stands in for the working pace once it is this old.
pub const FIVE_FALLBACK_AGE_S: i64 = 20 * 60;
/// The working pass's horizon.
pub const WORKING_HORIZON_S: i64 = 12 * 3600;
/// The most events one pass takes ([`Pass::cut_at`]).
pub const MAX_EVENTS: usize = 256;
/// A stop this short is the grace at a reset and is not reported.
pub const STOP_GRACE_S: i64 = 5 * 60;
/// A forecast instant at most this far ahead is said to [`ROUND_NEAR_S`] ([`round_down`]).
pub const NEAR_LEAD_S: i64 = 6 * 3600;
/// A forecast instant at most this far ahead (and further than [`NEAR_LEAD_S`]) is said to
/// [`ROUND_DAY_S`]; one further still to [`ROUND_FAR_S`].
pub const DAY_LEAD_S: i64 = 24 * 3600;
/// The grain of a forecast instant within [`NEAR_LEAD_S`].
pub const ROUND_NEAR_S: i64 = 600;
/// The grain of a forecast instant within [`DAY_LEAD_S`].
pub const ROUND_DAY_S: i64 = 1800;
/// The grain of a forecast instant beyond [`DAY_LEAD_S`]: a constant pace cannot place an
/// instant days ahead to the minute.
pub const ROUND_FAR_S: i64 = 3600;
/// How far back a row's own readings are kept for the glass ([`RowOutlook::read`]).
pub const READ_SPAN_S: i64 = 8 * 24 * 3600;
/// The most readings kept of one row ([`RowOutlook::read`]): one an hour over [`READ_SPAN_S`].
pub const READ_MAX: usize = 192;
/// A lookback rise of at least this many tenths (one point) on the carrier's scale is the
/// working pass's week rate; less, and the week's pace is.
pub const WORKING_WEEK_MIN_X10: i64 = 10;

const FULL_X10: i64 = 1000;
const HOUR: i64 = 3600;
const DAY: i64 = 86_400;

/// One of the two windows the outlook models.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Window {
    /// The 7-day week.
    Week,
    /// The 5-hour window.
    Five,
}

impl Window {
    /// The vendor's id ([`accounts::WEEK_LIMIT`], [`accounts::SESSION_LIMIT`]).
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Window::Week => accounts::WEEK_LIMIT,
            Window::Five => accounts::SESSION_LIMIT,
        }
    }

    /// Its length.
    #[must_use]
    pub const fn len_s(self) -> i64 {
        match self {
            Window::Week => accounts::WEEK_WINDOW_S,
            Window::Five => accounts::SESSION_WINDOW_S,
        }
    }

    /// The tenths at which the store's whole percent reaches the low threshold (half up).
    const fn low_x10(self) -> i64 {
        let pct = match self {
            Window::Week => accounts::LOW_WEEK_PCT,
            Window::Five => accounts::LOW_SESSION_PCT,
        };
        pct as i64 * 10 - 5
    }

    /// Its short word (`week`, `5h`).
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Window::Week => "week",
            Window::Five => "5h",
        }
    }
}

/// Where a row's plan came from ([`Plan::source`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PlanSource {
    /// The series' newest `plan` row, from the footer's gated profile read.
    Profile,
    /// The renewal check's `subscriptionType` (`pro`; a `max` names no tier).
    Renewal,
    /// Not seen.
    Assumed,
}

impl PlanSource {
    /// The JSON word.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            PlanSource::Profile => "profile",
            PlanSource::Renewal => "renewal",
            PlanSource::Assumed => "assumed",
        }
    }
}

/// A row's plan: its multiple (Anthropic's nominal per-session multiple: Pro 1, Max 5x 5, Max 20x
/// 20), which converts one account's percent onto another's scale and is never shown as a
/// capacity.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Plan {
    /// The multiple the replay counts: [`Self::named`] where the plan names one and the
    /// carrier's does too; the carrier's where this row's names none; 1 for every row where the
    /// carrier's names none (a common value cancels out, §5.1).
    pub m: u32,
    /// The multiple the plan names (a tier seen, or `pro`), where it names one: a `max` with no
    /// tier names none, since Max 5x and Max 20x are both `max`.
    pub named: Option<u32>,
    /// The plan's word, lower case (`max`), where seen.
    pub word: Option<String>,
    /// Its tier (`20x`), where seen.
    pub tier: Option<String>,
    /// Where it came from.
    pub source: PlanSource,
    /// When the series' `plan` row was written.
    pub at: Option<i64>,
}

impl Plan {
    /// The plan as a person reads it (`Max 20x`, `Pro`), `None` where not seen.
    #[must_use]
    pub fn title(&self) -> Option<String> {
        if self.source == PlanSource::Assumed {
            return None;
        }
        let word = self.word.as_deref()?;
        let mut title = String::new();
        let mut chars = word.chars();
        if let Some(first) = chars.next() {
            title.extend(first.to_uppercase());
            title.push_str(chars.as_str());
        }
        if let Some(tier) = &self.tier {
            title.push(' ');
            title.push_str(tier);
        }
        Some(title)
    }
}

/// The plan a footer subscription title names (`Max 20x` to `max`, `20x`; `Pro` to `pro`).
#[must_use]
pub fn plan_of_title(title: &str) -> Option<PlanSaid> {
    let mut words = title.split_whitespace();
    let word = words.next()?.to_ascii_lowercase();
    if !word.bytes().all(|b| b.is_ascii_alphabetic()) || word.len() > 16 {
        return None;
    }
    let tier = words
        .next()
        .filter(|t| tier_multiple(t).is_some())
        .map(str::to_ascii_lowercase);
    Some(PlanSaid {
        word,
        tier,
        src: "profile".into(),
    })
}

fn tier_multiple(tier: &str) -> Option<u32> {
    tier.strip_suffix(['x', 'X'])?
        .parse::<u32>()
        .ok()
        .filter(|m| (1..=1000).contains(m))
}

/// The multiple a plan names: its tier's, else `pro`'s 1. A `max` with no tier names none:
/// Max 5x and Max 20x are both `max`.
fn multiple_of(word: &str, tier: Option<&str>) -> Option<u32> {
    if let Some(m) = tier.and_then(tier_multiple) {
        return Some(m);
    }
    (word == "pro").then_some(1)
}

/// One window's freshness class (§6): what the card shows and what the replay may count.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Fresh {
    /// Read within 15 minutes, its reset ahead.
    Now,
    /// The carrier's, older than 15 minutes, at most an hour.
    AsOf,
    /// The carrier's, older than an hour while a session runs on it: its pace is refused.
    Stale,
    /// Its reset ahead; preferred here; nothing ran here since the reading. Idle *here* says
    /// nothing of another Mac, which may prefer it too (a preference is no claim), so the
    /// replay counts the reading as a lower bound, exactly as it does a pool row's.
    IdleHere,
    /// Its reset ahead; preferred here; a session ran here after the reading.
    UsedSince,
    /// Its reset ahead; not preferred here; older than 15 minutes: others may use it.
    Pool,
    /// Its reset passed since the reading: nothing is known of the new window until a read,
    /// since any Mac may have used it (a preference is no claim on an account).
    ResetUnread,
    /// No figure, wall or warning, ever.
    NeverRead,
    /// A reading exists, but its observation time cannot be admitted on this clock.
    ClockInvalid,
    /// A wall stands.
    Walled,
    /// Its login is signed out.
    SignedOut,
    /// No login was made for it here.
    NotEnrolled,
}

impl Fresh {
    /// The JSON word.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Fresh::Now => "now",
            Fresh::AsOf => "as-of",
            Fresh::Stale => "stale",
            Fresh::IdleHere => "idle-here",
            Fresh::UsedSince => "used-since",
            Fresh::Pool => "pool",
            Fresh::ResetUnread => "reset-unread",
            Fresh::NeverRead => "never-read",
            Fresh::ClockInvalid => "clock-invalid",
            Fresh::Walled => "walled",
            Fresh::SignedOut => "signed-out",
            Fresh::NotEnrolled => "not-enrolled",
        }
    }

    /// Whether the replay counts its figure exactly.
    const fn known(self) -> Known {
        match self {
            Fresh::Now | Fresh::AsOf | Fresh::Stale | Fresh::Walled => Known::Exact,
            Fresh::IdleHere | Fresh::UsedSince | Fresh::Pool => Known::Lower,
            Fresh::ResetUnread
            | Fresh::NeverRead
            | Fresh::ClockInvalid
            | Fresh::SignedOut
            | Fresh::NotEnrolled => Known::Unknown,
        }
    }

    /// Whether its figure is a bound: what was read, which any Mac may have spent since.
    const fn bounded(self) -> bool {
        matches!(self.known(), Known::Lower)
    }

    /// Whether a figure of this class may be shown as a number (never an unread one).
    #[must_use]
    pub const fn shows_figure(self) -> bool {
        matches!(
            self,
            Fresh::Now
                | Fresh::AsOf
                | Fresh::IdleHere
                | Fresh::UsedSince
                | Fresh::Pool
                | Fresh::Walled
        )
    }
}

/// How well the replay knows a figure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Known {
    Exact,
    Lower,
    Unknown,
}

/// Why a projection is refused: no dashes, no tick and no verdict, and the card and the CLI say why.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Refusal {
    /// No row has local account access on which the forecast can run.
    AccountUnavailable,
    /// The current account is unavailable and rotation is disabled.
    RotationOff,
    /// This pane's recovery is paused; the pool's observations remain available.
    SessionPaused,
    /// This pane's recovery observer could not confirm the current decision.
    ObserverBusy,
    /// The carrier has no current reading of the window.
    NoFigure,
    /// The carrier's reading is older than an hour while a session runs on it.
    Stale {
        /// Its age.
        age_s: i64,
    },
    /// The newest reading is stamped more than a minute ahead of this clock.
    Skew,
    /// The readings span less than the least span, and the fallback is not yet valid.
    TooEarly {
        /// The span read.
        have_s: i64,
        /// The span needed.
        need_s: i64,
    },
    /// No session was read in the last hour (the working pace only).
    Idle,
}

impl Refusal {
    /// The JSON word.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Refusal::AccountUnavailable => "account-unavailable",
            Refusal::RotationOff => "rotation-off",
            Refusal::SessionPaused => "session-paused",
            Refusal::ObserverBusy => "observer-busy",
            Refusal::NoFigure => "no-figure",
            Refusal::Stale { .. } => "stale",
            Refusal::Skew => "skew",
            Refusal::TooEarly { .. } => "too-early",
            Refusal::Idle => "idle",
        }
    }

    /// The words the card and the CLI say.
    #[must_use]
    pub fn words(self) -> String {
        match self {
            Refusal::AccountUnavailable => "account access unavailable".into(),
            Refusal::RotationOff => "rotation off".into(),
            Refusal::SessionPaused => "session recovery paused".into(),
            Refusal::ObserverBusy => "account recovery observer busy".into(),
            Refusal::NoFigure => "no reading".into(),
            Refusal::Stale { age_s } => {
                format!("not projected: read {} ago while in use", span_words(age_s))
            }
            Refusal::Skew => "reading ahead of this clock".into(),
            Refusal::TooEarly { have_s, need_s } => format!(
                "no pace yet: {} of readings, {} needed",
                span_words(have_s.max(0)),
                span_words(need_s)
            ),
            Refusal::Idle => "idle".into(),
        }
    }

    /// Its short words, the quota row's (`no pace yet`): [`Self::words`] without their detail.
    #[must_use]
    pub fn short(self) -> String {
        match self {
            Refusal::TooEarly { .. } => "no pace yet".into(),
            Refusal::Stale { .. } => "not projected".into(),
            other => other.words(),
        }
    }

    /// The spoken words.
    #[must_use]
    pub fn spoken(self) -> String {
        match self {
            Refusal::TooEarly { have_s, need_s } => format!(
                "No projection yet: {} of readings, {} needed.",
                spoken_span(have_s.max(0)),
                spoken_span(need_s)
            ),
            other => format!("No projection: {}.", other.words()),
        }
    }
}

/// What a pace stands on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Basis {
    /// The last 24 hours of rises, every account.
    Day,
    /// The last hour of rises, every account.
    Hour,
    /// The carrier's own week since it began (the series is younger than its least span).
    WeekBegan,
    /// The carrier's own 5-hour window since it opened.
    WindowOpened,
}

impl Basis {
    /// The JSON word.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Basis::Day => "day",
            Basis::Hour => "hour",
            Basis::WeekBegan => "week-began",
            Basis::WindowOpened => "window-opened",
        }
    }
}

/// A pace: tenths of a percent times plan multiple, per day (a day keeps a slow week's pace
/// exact in integers).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Pace {
    /// `x10 · M` per day; divided by a row's `M`, tenths of its window per day.
    pub x10_m_per_day: i64,
    /// What it stands on.
    pub basis: Basis,
    /// The span of readings it was made from.
    pub span_s: i64,
}

/// The work's demand.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Demand {
    /// The week's pace (today's pace).
    pub week: Result<Pace, Refusal>,
    /// The working pace (this hour's), over the 5-hour windows.
    pub five: Result<Pace, Refusal>,
    /// The working pass's week rate: the last hour's week rise where it reaches a point on the
    /// carrier's scale, else the week's pace.
    pub week_working: Option<i64>,
}

/// A row's projection of one window: what the replay spends of it by its reset.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Proj {
    /// Its tenths used at its reset (at most 1000).
    pub at_reset_x10: u32,
    /// When it fills before its reset, where it does.
    pub fills_at: Option<i64>,
    /// The pass that made it.
    pub pass: PassKind,
}

/// One window of one row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowOutlook {
    /// Which.
    pub window: Window,
    /// Its tenths used as read, where a figure is (never shown for an unread class).
    pub used_x10: Option<u32>,
    /// Its tenths used as last read, whatever its class ([`Self::used_x10`] keeps only a class
    /// the card shows as a number): the glass's figure, a bound until its reset
    /// ([`Figure::of_week`]).
    pub seen_x10: Option<u32>,
    /// Its reset as read (past for a reset class).
    pub reset: Option<i64>,
    /// When it was read.
    pub at: Option<i64>,
    /// Its class.
    pub fresh: Fresh,
    /// The replay's projection, where the chain uses it.
    pub projection: Option<Proj>,
    /// Where it was read.
    pub source: Option<Source>,
}

/// Where a figure was read: the tap, a `/usage` panel or a warning, on this machine or another.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Source {
    /// `tap`, `usage` or `warning`.
    pub kind: &'static str,
    /// Read on this machine.
    pub here: bool,
}

/// One roster row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RowOutlook {
    /// Its label.
    pub label: String,
    /// Preferred here or not ([`Class`]): an order, never a claim.
    pub class: Class,
    /// Its plan.
    pub plan: Plan,
    /// It is the store's `active`.
    pub active: bool,
    /// It is the carrier the replay starts on.
    pub carrier: bool,
    /// Its condition now.
    pub condition: Condition,
    /// When a session last ran on it here.
    pub ran_at: Option<i64>,
    /// Its week, and its 5-hour window.
    pub windows: Vec<WindowOutlook>,
    /// Where a wall stands on it now: the policy's answer at its wall.
    pub walled: Option<Walled>,
    /// Its week as this Mac read it over the last [`READ_SPAN_S`], oldest first: the last
    /// reading of each hour of each window, at most [`READ_MAX`] ([`Read`]).
    pub read: Vec<Read>,
}

impl RowOutlook {
    /// Its window `w`.
    #[must_use]
    pub fn window(&self, w: Window) -> Option<&WindowOutlook> {
        self.windows.iter().find(|x| x.window == w)
    }

    /// Its week's figure as the glass says it ([`Figure::of_week`]).
    #[must_use]
    pub fn figure(&self, as_of: i64) -> Figure {
        self.window(Window::Week)
            .map_or(Figure::Unread, |week| Figure::of_week(week, as_of))
    }
}

/// One reading of a row's week on this Mac ([`RowOutlook::read`]): a step the glass draws as
/// read, never interpolated.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Read {
    /// When it was read.
    pub t: i64,
    /// Its tenths used (0..=1000).
    pub x10: u16,
    /// Its window's vendor reset: a new reset is a new window.
    pub reset: i64,
    /// The used limit reset its window restarted at, where one did: a new epoch is a new window
    /// too, empty from that instant.
    pub epoch: Option<i64>,
}

/// A week figure as the glass says it (`docs/DESIGN-harness-glass-rainbow-2026-11-05.md` §2.1):
/// never an inferred 100%.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Figure {
    /// Read within the last 15 minutes, its reset ahead: `63%`.
    Exact(u16),
    /// A lower bound, true until its window's reset since use only rises within a window: an
    /// older reading (a stale carrier's too), a row idle here, used since, or a pool row's:
    /// `≥70%`, rounded down so it stays true.
    AtLeast(u16),
    /// A standing wall: `out`, never a percentage.
    Out,
    /// Unread, or read before a reset that has passed: `?`.
    Unread,
}

impl Figure {
    /// A reading's figure at `now`: exact while current, a lower bound after, unread once its
    /// window reset or where its clock cannot be admitted.
    #[must_use]
    pub fn of_reading(used_x10: u32, resets_at: i64, at: i64, now: i64) -> Figure {
        let Some(age) = super::profile::usage_read_age(at, now) else {
            return Figure::Unread;
        };
        if resets_at <= now {
            return Figure::Unread;
        }
        let x10 = u16::try_from(used_x10.min(1000)).unwrap_or(1000);
        if age <= super::profile::USAGE_CURRENT_S {
            Figure::Exact(x10)
        } else {
            Figure::AtLeast(x10)
        }
    }

    /// A footer's own weekly figure at `now` ([`Self::of_reading`]).
    #[must_use]
    pub fn of_weekly(figure: &super::profile::WeeklyFigure, now: i64) -> Figure {
        Figure::of_reading(figure.used_x10, figure.resets_at, figure.at, now)
    }

    /// An outlook row's week at `as_of`: a wall is [`Figure::Out`]; a reading of now exact; the
    /// last reading of any other class ([`WindowOutlook::seen_x10`]) a lower bound while its
    /// reset is ahead and its clock admitted (a stale carrier's too, whose pace is refused but
    /// whose figure stays true); else unread.
    #[must_use]
    pub fn of_week(week: &WindowOutlook, as_of: i64) -> Figure {
        if week.fresh == Fresh::Walled {
            return Figure::Out;
        }
        let (Some(x10), Some(reset), Some(at)) = (week.seen_x10, week.reset, week.at) else {
            return Figure::Unread;
        };
        match Figure::of_reading(x10, reset, at, as_of) {
            Figure::Exact(x10) if week.fresh != Fresh::Now => Figure::AtLeast(x10),
            figure => figure,
        }
    }

    /// Its words: `63%`, `≥70%`, `out`, `?`. A figure under 100 is never said as `100%`, and a
    /// bound is never rounded up past what was read.
    #[must_use]
    pub fn words(self) -> String {
        match self {
            Figure::Exact(x10) => format!("{}%", whole_used(x10)),
            Figure::AtLeast(x10) => format!("\u{2265}{}%", floor_used(x10)),
            Figure::Out => "out".into(),
            Figure::Unread => "?".into(),
        }
    }

    /// What a reader hears.
    #[must_use]
    pub fn spoken(self) -> String {
        match self {
            Figure::Exact(x10) => format!("{} percent used", whole_used(x10)),
            Figure::AtLeast(x10) => format!("at least {} percent used", floor_used(x10)),
            Figure::Out => "out until its reset".into(),
            Figure::Unread => "not read".into(),
        }
    }
}

/// Whole percent used, half up, but `100` only for a reading of 100.
fn whole_used(x10: u16) -> u16 {
    if x10 >= 1000 {
        100
    } else {
        ((x10 + 5) / 10).min(99)
    }
}

/// Whole percent used, rounded down: a lower bound stays true (62.5% is `≥62%`, never `≥63%`).
fn floor_used(x10: u16) -> u16 {
    x10.min(1000) / 10
}

/// The policy's answer at a standing wall.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Walled {
    /// The row it moves to at once.
    pub to: Option<String>,
    /// Else when the work resumes.
    pub until: Option<i64>,
    /// The vendor reset `until` is the grace after, where a standing wall (on any row) names
    /// one and stands exactly to it: `until` is that reset plus [`accounts::RESET_GRACE_S`], and
    /// the glass says the reset, exactly (`docs/DESIGN-harness-glass-rainbow-2026-11-05.md`
    /// §2.1), whichever window's wall it is.
    pub reset: Option<i64>,
}

/// The vendor reset whose grace a standing wall ends at `until`, on any row of `state`: the
/// lookup `begin_stop` makes for a planned stop, here for a wait standing now.
fn wall_reset(state: &State, until: i64) -> Option<i64> {
    state
        .accounts
        .values()
        .flat_map(|a| a.walls.values())
        .filter(|wall| wall.stands_until() == Some(until))
        .find_map(|wall| {
            wall.until
                .filter(|reset| reset.saturating_add(accounts::RESET_GRACE_S) == until)
        })
}

/// Which pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PassKind {
    /// Seven days at the week's pace.
    Week,
    /// Twelve hours at the working pace, the 5-hour windows modeled.
    Working,
}

impl PassKind {
    /// The JSON word.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            PassKind::Week => "week",
            PassKind::Working => "working",
        }
    }
}

/// How a link of the chain ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Ends {
    /// The rotation leaves it ahead of its wall.
    Leaves,
    /// It walls.
    Walls,
    /// The horizon.
    Horizon,
}

impl Ends {
    /// The JSON word.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Ends::Leaves => "leaves",
            Ends::Walls => "walls",
            Ends::Horizon => "horizon",
        }
    }
}

/// One link of the chain: a row carrying the work.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    /// The row.
    pub label: String,
    /// From.
    pub from: i64,
    /// To.
    pub to: i64,
    /// How it ends.
    pub ends: Ends,
    /// Its tenths used when it ends, of its week in the week pass and of its 5-hour window in
    /// the working pass.
    pub left_at_x10: Option<u32>,
}

/// Where a stop ends: the first event past its start on which the policy resumes the work or
/// moves it, followed past the pass's horizon to [`Pass::horizon`] plus a week at most.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StopEnd {
    /// At this vendor reset of the account the work resumes on (the work resumes at its grace's
    /// end): said exactly.
    Reset(i64),
    /// At this forecast instant: said `~`, rounded up by its lead.
    About(i64),
    /// Nothing ends it within a week past the horizon.
    Open,
}

impl StopEnd {
    /// The instant it names, where it names one.
    #[must_use]
    pub const fn at(self) -> Option<i64> {
        match self {
            StopEnd::Reset(t) | StopEnd::About(t) => Some(t),
            StopEnd::Open => None,
        }
    }

    /// The JSON word.
    #[must_use]
    pub const fn kind(self) -> &'static str {
        match self {
            StopEnd::Reset(_) => "reset",
            StopEnd::About(_) => "about",
            StopEnd::Open => "open",
        }
    }
}

/// A stop: every account out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stop {
    /// From.
    pub from: i64,
    /// Where it ends.
    pub end: StopEnd,
    /// The window that walled.
    pub window: Window,
    /// The work would go on on usage credits (R-9's switch on).
    pub credits: bool,
}

/// One knot of a row's fill in a pass ([`Pass::fills`]): its tenths used of the pass's window
/// (the week in the week pass, the 5-hour window in the working pass) at `t`, `None` where the
/// replay does not know it. The fill is linear between knots; a drop is two knots at one `t`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Knot {
    /// When.
    pub t: i64,
    /// Its tenths used (0..=1000), where known.
    pub x10: Option<u16>,
}

/// A fork at an account aterm cannot read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lean {
    /// When the policy moves onto it.
    pub at: i64,
    /// The row.
    pub row: String,
    /// Its class.
    pub why: Fresh,
    /// It holds nothing.
    pub worst: Pass,
    /// It holds its read figure, or a fresh week.
    pub best: Pass,
}

/// One pass of the replay.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pass {
    /// Which.
    pub kind: PassKind,
    /// Its horizon.
    pub horizon: i64,
    /// The chain, in order.
    pub chain: Vec<Link>,
    /// The stops.
    pub stops: Vec<Stop>,
    /// The fork, where one was met.
    pub lean: Option<Box<Lean>>,
    /// Where the event bound cut it.
    pub cut_at: Option<i64>,
    /// The events it took.
    pub events: usize,
    /// Each used row window's projection: its first window's, at its reset.
    pub proj: BTreeMap<(String, Window), Proj>,
    /// Each row's fill through the pass, up to its horizon (or its fork), oldest first: a knot
    /// wherever it changes course (the carrier's after each step, a reset's drop, a move, the
    /// worst case's assumption, the end), so a later window's run-out has its curve too. All rows
    /// together hold at most [`MAX_EVENTS`] knots for each row.
    pub fills: BTreeMap<String, Vec<Knot>>,
}

impl Pass {
    /// The projections a bar shows: this pass's, and past its lean the worst case's.
    #[must_use]
    pub fn projections(&self) -> BTreeMap<(String, Window), Proj> {
        let mut out = self.proj.clone();
        if let Some(lean) = &self.lean {
            for (key, proj) in lean.worst.projections() {
                out.entry(key).or_insert(proj);
            }
        }
        out
    }

    /// The first reported stop of this pass's own chain (not its lean's).
    #[must_use]
    pub fn first_stop(&self) -> Option<&Stop> {
        self.stops.first()
    }

    /// Every label the chain uses, its lean's worst branch included.
    fn used(&self, out: &mut BTreeSet<String>) {
        out.extend(self.chain.iter().map(|l| l.label.clone()));
        if let Some(lean) = &self.lean {
            lean.worst.used(out);
            lean.best.used(out);
        }
    }
}

/// The verdict the footer, the card and the CLI say.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Verdict {
    /// The carrier is walled now.
    Walled {
        /// The row the policy moves to at once.
        to: Option<String>,
        /// Else when the work resumes.
        until: Option<i64>,
    },
    /// A stop in a pass.
    Stop {
        /// Which pass.
        pass: PassKind,
        /// From.
        from: i64,
        /// Where it ends.
        end: StopEnd,
        /// Both branches of a lean stop: the row it leans on.
        lean: Option<String>,
    },
    /// The worst case of a lean stops, the best does not.
    Lean {
        /// The work is covered to here.
        until: i64,
        /// Then the rotation moves to this row, which aterm cannot read.
        row: String,
        /// Its class.
        why: Fresh,
        /// The worst case's stop: from, and where it ends (never at the wall the worst case
        /// invented).
        stop: Option<(i64, StopEnd)>,
    },
    /// Usage credits carry a stop ahead (R-9's switch on).
    Credits {
        /// From.
        at: i64,
    },
    /// No stop in the next 7 days.
    Continuous,
    /// The pace is refused.
    Refused(Refusal),
}

impl Verdict {
    /// The JSON kind.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Verdict::Walled { .. } => "walled",
            Verdict::Stop {
                pass: PassKind::Working,
                ..
            } => "stop-5h",
            Verdict::Stop { .. } => "stop",
            Verdict::Lean { .. } => "lean",
            Verdict::Credits { .. } => "credits",
            Verdict::Continuous => "continuous",
            Verdict::Refused(_) => "refused",
        }
    }

    /// Whether it says the work stops (said in warn ink, with its mark and words: `╳` for a
    /// projected stop, `⧗` for a wall that waits now).
    #[must_use]
    pub const fn warns(&self) -> bool {
        matches!(
            self,
            Verdict::Stop { .. } | Verdict::Walled { to: None, .. }
        )
    }
}

/// An assumption the outlook made (`≈` in the card and the CLI).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Approx {
    /// A row's plan names no multiple (not seen, or a `max` with no tier): counted as the
    /// carrier's.
    PlanAssumed {
        /// The row.
        label: String,
        /// Its plan's word where one was seen (`max`).
        word: Option<String>,
        /// The carrier's plan title.
        like: Option<String>,
        /// The carrier.
        carrier: String,
    },
    /// The carrier's plan names no multiple: every row is counted alike.
    PlansAlike {
        /// The carrier.
        carrier: String,
    },
    /// Rows of different plans: one account's week is put on another's scale by the plans'
    /// 5-hour multiples, which Anthropic states per 5-hour session, not per week.
    MultiplesScaled,
}

/// The whole outlook.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outlook {
    /// When it was made.
    pub as_of: i64,
    /// The local clock its words are said on, each instant on its own offset.
    pub offsets: Offsets,
    /// The rotation's mode.
    pub mode: Mode,
    /// `[harness] usage_credits`.
    pub credits: bool,
    /// The row the replay starts on.
    pub carrier: Option<String>,
    /// The work's demand.
    pub demand: Demand,
    /// Every roster row, in roster order.
    pub rows: Vec<RowOutlook>,
    /// The week pass, where the pace stood.
    pub week: Option<Pass>,
    /// The working pass, where the working pace stood.
    pub working: Option<Pass>,
    /// The verdict.
    pub verdict: Verdict,
    /// The assumptions made.
    pub approx: Vec<Approx>,
    /// The enrolled rows no pass uses.
    pub not_needed: Vec<String>,
}

/// What [`outlook`] reads: nothing but what aterm already observed.
#[derive(Clone, Copy, Debug)]
pub struct Inputs<'a> {
    /// The roster, bound.
    pub roster: &'a Roster,
    /// The accounts state, as read; never written.
    pub state: &'a State,
    /// The authoritative enrolled labels, including any enrolled default-home row.
    pub enrolled: &'a BTreeSet<String>,
    /// Each label's series rows, oldest first.
    pub series: &'a BTreeMap<String, Vec<SeriesRow>>,
    /// Each label's renewal `subscriptionType`, where the logins hold one.
    pub renewals: &'a BTreeMap<String, String>,
    /// `[harness] accounts`.
    pub mode: Mode,
    /// `[harness] usage_credits`.
    pub credits: bool,
    /// Now.
    pub now: i64,
    /// The local clock's offset from UTC at an instant: its words say each instant on its own
    /// offset ([`Offsets::around`]).
    pub offset: fn(i64) -> i64,
}

/// How far either side of now an outlook says instants on their own offsets: the week ahead and
/// the series' eight days back ([`super::account_series::VIEW_S`]), with a day to spare.
const OFFSETS_SPAN_S: i64 = 9 * DAY;

/// The step [`Offsets::around`] samples a zone at, 72 looks a build: shorter than any gap
/// between two changes of its offset.
const OFFSETS_STEP_S: i64 = 6 * HOUR;

/// The local clock an outlook's words are said on: each instant's own offset from UTC, so a
/// reset or a stop past a daylight-saving change reads on the clock it happens on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Offsets {
    /// `(from, offset)`, ascending; the first holds before its instant too.
    steps: Vec<(i64, i64)>,
}

impl Offsets {
    /// One offset for every instant.
    #[must_use]
    pub fn fixed(offset_s: i64) -> Offsets {
        Offsets {
            steps: vec![(i64::MIN, offset_s)],
        }
    }

    /// `offset_at`'s offsets from nine days before `now` to nine days after: sampled every
    /// [`OFFSETS_STEP_S`], each change found to the second. Outside that span the nearest end's
    /// offset holds.
    #[must_use]
    pub fn around(now: i64, offset_at: impl Fn(i64) -> i64) -> Offsets {
        let Ok(offsets) =
            Offsets::sampled(now, |t| Ok::<_, std::convert::Infallible>(offset_at(t)));
        offsets
    }

    /// [`Offsets::around`] for a zone that may not answer (`aterm_uds::zone::local_offset_at` on
    /// a platform it cannot read): `None` where `offset_at` says nothing at any instant it is
    /// asked, so the caller says no local time rather than another zone's.
    #[must_use]
    pub fn read_around(now: i64, offset_at: impl Fn(i64) -> Option<i64>) -> Option<Offsets> {
        Offsets::sampled(now, |t| offset_at(t).ok_or(())).ok()
    }

    /// [`Offsets::around`]'s sampling, stopped at the first instant `offset_at` fails on.
    fn sampled<E>(now: i64, offset_at: impl Fn(i64) -> Result<i64, E>) -> Result<Offsets, E> {
        let mut t = now - OFFSETS_SPAN_S;
        let mut was = offset_at(t)?;
        let mut steps = vec![(i64::MIN, was)];
        while t < now + OFFSETS_SPAN_S {
            let next = t + OFFSETS_STEP_S;
            let then = offset_at(next)?;
            if then != was {
                // The first second after t, up to and including next, on the new offset.
                let (mut lo, mut hi) = (t, next);
                while hi - lo > 1 {
                    let mid = lo + (hi - lo) / 2;
                    if offset_at(mid)? == was {
                        lo = mid;
                    } else {
                        hi = mid;
                    }
                }
                steps.push((hi, then));
                was = then;
            }
            t = next;
        }
        Ok(Offsets { steps })
    }

    /// The offset at `t`.
    #[must_use]
    pub fn at(&self, t: i64) -> i64 {
        let i = self.steps.partition_point(|(from, _)| *from <= t);
        self.steps
            .get(i.saturating_sub(1))
            .map_or(0, |(_, offset)| *offset)
    }

    /// The first instant after `t` at which the offset changes, where one does.
    #[must_use]
    pub fn change_after(&self, t: i64) -> Option<i64> {
        let i = self.steps.partition_point(|(from, _)| *from <= t);
        self.steps.get(i).map(|(from, _)| *from)
    }
}

// ---------------------------------------------------------------------------------------------
// Figures and classes

/// A window's newest time-admitted tap/sample figure, else an admitted warning,
/// whatever its reset. Raw clock-invalid evidence survives only when no source
/// is admitted, so its unknown clock is distinct from an unread window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Seen {
    pct_x10: u32,
    reset: i64,
    at: i64,
    /// Where it was read ([`Source`]).
    source: Source,
}

fn seen_of(acct: &accounts::Account, w: Window, machine: &str, now: i64) -> Option<Seen> {
    let tap = acct.windows.get(w.id()).filter(|f| !f.cut()).map(|f| Seen {
        pct_x10: f.pct_x10.min(1000),
        reset: f.reset,
        at: f.at,
        source: Source {
            kind: if f.source == accounts::SOURCE_SAMPLE {
                "usage"
            } else {
                "tap"
            },
            here: f.by == machine,
        },
    });
    let sample = acct.sample.as_ref().and_then(|s| {
        let (pct, reset, at) = match w {
            Window::Week => (s.week_pct?, s.week_reset?, s.week_read_at.unwrap_or(s.at)),
            Window::Five => (
                s.session_pct?,
                s.session_reset?,
                s.session_read_at.unwrap_or(s.at),
            ),
        };
        Some(Seen {
            pct_x10: pct.saturating_mul(10).min(1000),
            reset,
            at,
            source: Source {
                kind: "usage",
                here: s.by == machine,
            },
        })
    });
    let warning = (|| {
        let warned = match w {
            Window::Week => acct.warned_week.as_ref(),
            Window::Five => acct.warned_session.as_ref(),
        }?;
        Some(Seen {
            pct_x10: warned.pct?.saturating_mul(10).min(1000),
            reset: warned.reset?,
            at: warned.at,
            source: Source {
                kind: "warning",
                here: true,
            },
        })
    })();
    let newer = |a: Option<Seen>, b: Option<Seen>| match (a, b) {
        (Some(a), Some(b)) => Some(if b.at > a.at { b } else { a }),
        (a, b) => a.or(b),
    };
    let admitted = |reading: Option<Seen>| {
        reading.filter(|reading| {
            super::profile::usage_read_age(reading.at, now).is_some()
                && acct
                    .windows
                    .get(w.id())
                    .is_none_or(|window| window.admits_reading(reading.at))
        })
    };
    let clock_invalid = |reading: Option<Seen>| {
        reading.filter(|reading| super::profile::usage_read_age(reading.at, now).is_none())
    };
    // An invalid newer source cannot hide admissible history. If none is admitted,
    // retain clock-invalid evidence to distinguish it from an unread window. A
    // reading before the refill barrier remains cut, as it was before fallback.
    newer(admitted(tap), admitted(sample))
        .or_else(|| admitted(warning))
        .or_else(|| newer(clock_invalid(tap), clock_invalid(sample)))
        .or_else(|| clock_invalid(warning))
}

fn ran_here_since(state: &State, label: &str, since: i64) -> bool {
    state
        .accounts
        .get(label)
        .and_then(|a| a.ran_at)
        .is_some_and(|ran| ran > since)
        || state
            .sessions
            .values()
            .any(|s| s.account == label && s.at > since)
}

fn in_use(state: &State, label: &str) -> bool {
    state.sessions.values().any(|s| s.account == label)
}

/// The class of `label`'s window `w` at `now` ([`Fresh`]).
fn fresh_of(
    row: &accounts::Row,
    state: &State,
    cond: Condition,
    carrier: bool,
    w: Window,
    now: i64,
) -> (Fresh, Option<Seen>) {
    let acct = state.accounts.get(&row.label);
    let seen = acct.and_then(|a| seen_of(a, w, &state.machine, now));
    let fresh = match cond {
        Condition::NotEnrolled => Fresh::NotEnrolled,
        Condition::SignedOut => Fresh::SignedOut,
        // A wall walls its own window; a wall that names no window, or another week's (a
        // model's), walls the week. A 5-hour wall never does: the week keeps its figure.
        Condition::Limited { .. }
            if acct.is_some_and(|a| {
                a.walls.iter().any(|(id, wall)| {
                    let walls_w = id == w.id()
                        || (w == Window::Week && id.as_str() != accounts::SESSION_LIMIT);
                    walls_w && wall.stands_until().is_some_and(|end| now < end)
                })
            }) =>
        {
            Fresh::Walled
        }
        _ => match seen {
            None => Fresh::NeverRead,
            Some(s) if super::profile::usage_read_age(s.at, now).is_none() => Fresh::ClockInvalid,
            Some(s) if s.reset > now => {
                let age = now.saturating_sub(s.at);
                if age <= super::profile::USAGE_CURRENT_S {
                    Fresh::Now
                } else if carrier && age <= super::profile::USAGE_SHOWN_S {
                    Fresh::AsOf
                } else if carrier && in_use(state, &row.label) {
                    Fresh::Stale
                } else if row.class == Class::Pool {
                    Fresh::Pool
                } else if ran_here_since(state, &row.label, s.at) {
                    Fresh::UsedSince
                } else {
                    Fresh::IdleHere
                }
            }
            // Nothing here running it since the reset says nothing of another Mac's use.
            Some(_) => Fresh::ResetUnread,
        },
    };
    (fresh, seen)
}

/// The next reset after `now` of a window that reset at `passed`, a whole number of windows on.
fn next_reset(passed: i64, len: i64, now: i64) -> i64 {
    if passed > now {
        return passed;
    }
    let k = (now - passed) / len + 1;
    passed + k * len
}

// ---------------------------------------------------------------------------------------------
// Pace (§5)

/// `w`'s runs in `rows` (§5.2, [`account_series::runs`]), of the figures the display admits at
/// `now` ([`super::profile::usage_read_age`]).
fn runs<'a>(rows: &'a [SeriesRow], w: Window, now: i64) -> Vec<Run<'a>> {
    account_series::runs(
        rows.iter()
            .filter_map(|r| r.fig(w.id()))
            .filter(|figure| super::profile::usage_read_age(figure.at, now).is_some()),
    )
}

/// The rise of every label's `w` runs that touch `[now − lookback, now]`, weighted by plan
/// multiple: `(units, earliest start, any row at or before the lookback's start)`.
fn rise(
    series: &BTreeMap<String, Vec<SeriesRow>>,
    plans: &BTreeMap<String, u32>,
    default_m: u32,
    w: Window,
    lookback: i64,
    now: i64,
) -> (i64, Option<i64>, bool) {
    let a = now - lookback;
    let mut units = 0i64;
    let mut earliest: Option<i64> = None;
    let mut before = false;
    for (label, rows) in series {
        if rows.iter().any(|r| {
            r.t <= a
                && r.fig(w.id())
                    .is_some_and(|figure| super::profile::usage_read_age(figure.at, now).is_some())
        }) {
            before = true;
        }
        let m = i64::from(plans.get(label).copied().unwrap_or(default_m));
        for run in runs(rows, w, now) {
            let (Some(first), Some(last)) = (run.figs.first(), run.figs.last()) else {
                continue;
            };
            if last.at < a || first.at > now {
                continue;
            }
            let empty_at = match run.began {
                Began::Window => Some(last.reset - w.len_s()),
                Began::Epoch(epoch) => Some(epoch),
                Began::Refill => None,
            };
            let (start_pct, start_at) = match run.figs.iter().rposition(|f| f.at <= a) {
                Some(i) => at_or_before(&run.figs, i, a),
                None => match empty_at.filter(|at| *at >= a && *at <= first.at) {
                    Some(at) => (0, at),
                    None => (first.pct_x10, first.at),
                },
            };
            let end = run
                .figs
                .iter()
                .rev()
                .find(|f| f.at <= now)
                .map_or(last.pct_x10, |f| f.pct_x10);
            units += m * i64::from(end.saturating_sub(start_pct));
            earliest = Some(earliest.map_or(start_at, |e: i64| e.min(start_at)));
        }
    }
    (units, earliest, before)
}

/// A run's value at `a`, from its row `i` at or before `a`: read linearly towards the next row
/// when one follows, so a gap's rise is spread over the gap and not charged to the lookback.
fn at_or_before(figs: &[&account_series::Fig], i: usize, a: i64) -> (u32, i64) {
    let Some(f) = figs.get(i) else {
        return (0, a);
    };
    match figs.get(i + 1) {
        Some(g) if f.at < a && g.at > f.at => {
            let (fp, gp) = (i64::from(f.pct_x10), i64::from(g.pct_x10));
            let v = fp + (gp - fp) * (a - f.at) / (g.at - f.at);
            (u32::try_from(v.max(0)).unwrap_or(f.pct_x10), a)
        }
        _ => (f.pct_x10, f.at),
    }
}

#[allow(clippy::too_many_arguments)]
fn pace_of(
    series: &BTreeMap<String, Vec<SeriesRow>>,
    plans: &BTreeMap<String, u32>,
    carrier_m: u32,
    carrier: Option<Seen>,
    w: Window,
    (lookback, least, fallback_age): (i64, i64, i64),
    basis: Basis,
    now: i64,
) -> Result<Pace, Refusal> {
    let (units, earliest, before) = rise(series, plans, carrier_m, w, lookback, now);
    let span = if before {
        lookback
    } else {
        earliest.map_or(0, |e| now - e)
    };
    if span >= least && span > 0 {
        return Ok(Pace {
            x10_m_per_day: units * DAY / span,
            basis,
            span_s: span,
        });
    }
    if let Some(c) = carrier {
        let began = c.reset - w.len_s();
        let age = c.at - began;
        if age >= fallback_age && age > 0 {
            return Ok(Pace {
                x10_m_per_day: i64::from(carrier_m) * i64::from(c.pct_x10) * DAY / age,
                basis: match w {
                    Window::Week => Basis::WeekBegan,
                    Window::Five => Basis::WindowOpened,
                },
                span_s: age,
            });
        }
    }
    Err(Refusal::TooEarly {
        have_s: span,
        need_s: least,
    })
}

// ---------------------------------------------------------------------------------------------
// The replay (§7)

/// One modeled window of one row in a pass: its truth, in tenths times `86400 · M(row)` so a pace
/// in `x10 · M` per day adds exactly, per second.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Win {
    acc: i128,
    reset: Option<i64>,
    known: Known,
    low_fired: bool,
    fills_at: Option<i64>,
}

impl Win {
    /// An empty window with no reset, known as `known`.
    const fn empty(known: Known) -> Win {
        Win {
            acc: 0,
            reset: None,
            known,
            low_fired: false,
            fills_at: None,
        }
    }
}

#[derive(Clone, Debug)]
struct SimRow {
    label: String,
    m: u32,
    seen: bool,
    week: Win,
    five: Win,
    fresh_week: Fresh,
    /// The read figure a best case counts ([`Known::Lower`]).
    read_week: Option<Seen>,
}

impl SimRow {
    fn scale(&self) -> i128 {
        i128::from(self.m.max(1)) * i128::from(DAY)
    }

    fn used_x10(&self, w: &Win) -> u32 {
        u32::try_from((w.acc / self.scale()).clamp(0, i128::from(FULL_X10))).unwrap_or(1000)
    }

    fn win(&self, w: Window) -> &Win {
        match w {
            Window::Week => &self.week,
            Window::Five => &self.five,
        }
    }

    fn win_mut(&mut self, w: Window) -> &mut Win {
        match w {
            Window::Week => &mut self.week,
            Window::Five => &mut self.five,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Branch {
    Main,
    Worst,
    Best,
}

#[derive(Clone, Copy)]
struct Ctx<'a> {
    roster: &'a Roster,
    enrolled: &'a BTreeSet<String>,
    mode: Mode,
    credits: bool,
    kind: PassKind,
    horizon: i64,
    /// A stop open at the horizon is followed to here at most: [`Self::horizon`] plus a week.
    resume_bound: i64,
    /// `x10 · M` per day, per modeled window.
    week_rate: i64,
    five_rate: Option<i64>,
}

impl Ctx<'_> {
    fn modeled(&self) -> &'static [Window] {
        match self.kind {
            PassKind::Week => &[Window::Week],
            PassKind::Working => &[Window::Week, Window::Five],
        }
    }

    /// The window a pass's fills and links say: the week, or the working pass's 5 hours.
    const fn filled(&self) -> Window {
        match self.kind {
            PassKind::Week => Window::Week,
            PassKind::Working => Window::Five,
        }
    }

    fn rate(&self, w: Window) -> i64 {
        match w {
            Window::Week => self.week_rate,
            Window::Five => self.five_rate.unwrap_or(0),
        }
        .max(0)
    }
}

/// A stop in progress ([`Sim::begin_stop`]).
#[derive(Clone, Debug)]
struct Planned {
    from: i64,
    /// When the policy said the work may go on, unclamped: it may lie past the horizon.
    until: i64,
    /// The window that walled.
    window: Window,
    /// Each row whose real wall stands exactly to [`Self::until`], with the vendor reset that
    /// instant is the grace of.
    resets: Vec<(usize, i64)>,
}

#[derive(Clone)]
struct Sim<'a> {
    ctx: Ctx<'a>,
    state: State,
    rows: Vec<SimRow>,
    carrier: usize,
    t: i64,
    link_from: i64,
    /// A stop in progress.
    stop: Option<Planned>,
    /// Every decision taken, where a test asked for them ([`Decision`]).
    trace: Option<std::rc::Rc<std::cell::RefCell<Vec<Decision>>>>,
    chain: Vec<Link>,
    stops: Vec<Stop>,
    proj: BTreeMap<(String, Window), Proj>,
    /// [`Pass::fills`], and the knots in it.
    fills: BTreeMap<String, Vec<Knot>>,
    knots: usize,
    /// The fills reached the horizon (or the fork): no knot follows.
    fills_closed: bool,
    events: usize,
    cut_at: Option<i64>,
    branch: Branch,
    lean: Option<Box<Lean>>,
}

fn me(label: &str) -> Me<'_> {
    Me {
        account: Some(label),
        left: None,
        default_home: true,
        route: crate::harness::accounts::AuthRoute::Stored,
    }
}

impl<'a> Sim<'a> {
    fn carrier_label(&self) -> String {
        self.rows[self.carrier].label.clone()
    }

    fn index(&self, label: &str) -> Option<usize> {
        self.rows.iter().position(|r| r.label == label)
    }

    /// Writes row `i`'s modeled figures into the clone through the tap's own writer.
    fn read_into_state(&mut self, i: usize) {
        let row = &self.rows[i];
        let read = |w: Window| {
            let win = row.win(w);
            (self.ctx.modeled().contains(&w) && win.known == Known::Exact)
                .then_some(())
                .and(win.reset)
                .filter(|reset| *reset > self.t)
                .map(|reset| WindowRead {
                    pct_x10: row.used_x10(win),
                    reset,
                    refilled: None,
                })
        };
        let read = TapRead {
            five_hour: read(Window::Five),
            seven_day: read(Window::Week),
            at: self.t,
            seen_before: true,
            born: None,
            key: None,
        };
        let label = row.label.clone();
        let _ = accounts::note_reading_held(&mut self.state, &label, &read, self.t);
    }

    fn advise(&self, point: &Point) -> Advice {
        let label = self.carrier_label();
        let advice = accounts::advise_past_probe(
            self.ctx.roster,
            &self.state,
            self.ctx.enrolled,
            &me(&label),
            point,
            self.ctx.mode,
            self.t,
        );
        if let Some(trace) = &self.trace {
            let row = &self.rows[self.carrier];
            let modeled = self
                .ctx
                .modeled()
                .iter()
                .filter_map(|&w| {
                    let win = row.win(w);
                    let reset = win.reset.filter(|r| *r > self.t)?;
                    (win.known == Known::Exact).then(|| (w, row.used_x10(win), reset))
                })
                .collect();
            trace.borrow_mut().push(Decision {
                state: self.state.clone(),
                carrier: label,
                point: point.clone(),
                t: self.t,
                horizon: self.ctx.horizon,
                advice: advice.clone(),
                modeled,
            });
        }
        advice
    }

    fn close_link(&mut self, ends: Ends) {
        let row = &self.rows[self.carrier];
        self.chain.push(Link {
            label: row.label.clone(),
            from: self.link_from,
            to: self.t,
            ends,
            left_at_x10: Some(row.used_x10(row.win(self.ctx.filled()))),
        });
        self.link_from = self.t;
    }

    /// Row `i`'s fill of the pass's window now, where the replay knows it.
    fn fill_of(&self, i: usize) -> Option<u16> {
        let row = &self.rows[i];
        let win = row.win(self.ctx.filled());
        (win.known != Known::Unknown).then(|| u16::try_from(row.used_x10(win)).unwrap_or(1000))
    }

    /// A knot of row `i`'s fill at `t` ([`Pass::fills`]): none past the horizon or the fills'
    /// close, nor past [`MAX_EVENTS`] for each row, all rows counted; a repeat is dropped, and
    /// a flat run keeps only its ends.
    fn push_knot(&mut self, i: usize, t: i64, x10: Option<u16>) {
        if self.fills_closed
            || t > self.ctx.horizon
            || self.knots >= MAX_EVENTS.saturating_mul(self.rows.len())
        {
            return;
        }
        let knot = Knot { t, x10 };
        let knots = self.fills.entry(self.rows[i].label.clone()).or_default();
        let n = knots.len();
        if n >= 1 && knots[n - 1] == knot {
            return;
        }
        if n >= 2
            && knots[n - 2].x10 == x10
            && knots[n - 1].x10 == x10
            && knots[n - 2].t < knots[n - 1].t
            && knots[n - 1].t <= t
        {
            knots[n - 1].t = t;
            return;
        }
        knots.push(knot);
        self.knots += 1;
    }

    /// A knot of row `i`'s fill now.
    fn note_fill(&mut self, i: usize) {
        let x10 = self.fill_of(i);
        self.push_knot(i, self.t, x10);
    }

    /// A knot of every row's fill now, at most at the horizon, and no knot after
    /// ([`Pass::fills`] ends here).
    fn close_fills(&mut self) {
        if self.fills_closed {
            return;
        }
        let t = self.t.min(self.ctx.horizon);
        for i in 0..self.rows.len() {
            let x10 = self.fill_of(i);
            self.push_knot(i, t, x10);
        }
        self.fills_closed = true;
    }

    /// A pass's (or a branch's) fills begin: every row's knot now.
    fn seed_fills(&mut self) {
        self.fills.clear();
        self.knots = 0;
        self.fills_closed = false;
        for i in 0..self.rows.len() {
            self.note_fill(i);
        }
    }

    fn capture(&mut self, i: usize, w: Window) {
        let row = &self.rows[i];
        let key = (row.label.clone(), w);
        if row.seen && !self.proj.contains_key(&key) {
            let win = row.win(w);
            self.proj.insert(
                key,
                Proj {
                    at_reset_x10: row.used_x10(win),
                    fills_at: win.fills_at,
                    pass: self.ctx.kind,
                },
            );
        }
    }

    /// The earliest event after `t` (R4): at most the horizon, or while a stop is open, the
    /// resume bound. The worst case's invented wall and reset lie at or past the bound
    /// ([`Self::assume_worst`]), so neither is ever an event.
    fn next_event(&self) -> i64 {
        let t = self.t;
        let mut next = if self.stop.is_some() {
            self.ctx.resume_bound
        } else {
            self.ctx.horizon
        };
        let mut at = |e: i64| {
            if e > t && e < next {
                next = e;
            }
        };
        if let Some(stop) = &self.stop {
            at(stop.until);
        } else {
            let row = &self.rows[self.carrier];
            for &w in self.ctx.modeled() {
                let rate = i128::from(self.ctx.rate(w));
                let win = row.win(w);
                if rate <= 0 {
                    continue;
                }
                let mut target = |x10: i64| {
                    let goal = i128::from(x10) * row.scale();
                    if win.acc < goal {
                        let dt = (goal - win.acc + rate - 1) / rate;
                        at(t.saturating_add(i64::try_from(dt).unwrap_or(i64::MAX)));
                    }
                };
                if self.ctx.mode == Mode::Early && !win.low_fired {
                    target(w.low_x10());
                }
                target(FULL_X10);
            }
        }
        for row in &self.rows {
            for &w in self.ctx.modeled() {
                let win = row.win(w);
                if let Some(reset) = win.reset {
                    at(reset);
                }
            }
        }
        for acct in self.state.accounts.values() {
            for wall in acct.walls.values() {
                if let Some(end) = wall.stands_until() {
                    at(end);
                }
            }
        }
        next
    }

    fn advance(&mut self, to: i64) {
        let dt = i128::from(to.saturating_sub(self.t));
        if self.stop.is_some() || dt <= 0 {
            return;
        }
        let c = self.carrier;
        self.note_fill(c);
        self.spend(to, dt);
        let x10 = self.fill_of(c);
        self.push_knot(c, to, x10);
    }

    /// The carrier spends the `dt` seconds to `to` at the work's pace, at most to full.
    fn spend(&mut self, to: i64, dt: i128) {
        let modeled = self.ctx.modeled();
        let rates: Vec<(Window, i128)> = modeled
            .iter()
            .map(|w| (*w, i128::from(self.ctx.rate(*w))))
            .collect();
        let row = &mut self.rows[self.carrier];
        let full = i128::from(FULL_X10) * row.scale();
        for (w, rate) in rates {
            let win = row.win_mut(w);
            if win.reset.is_none() {
                continue;
            }
            win.acc = (win.acc + rate * dt).min(full);
            if win.acc >= full && win.fills_at.is_none() {
                win.fills_at = Some(to);
            }
        }
    }

    /// R8: each modeled window whose reset is now; a row whose filled window resets drops in
    /// [`Pass::fills`]: two knots at one instant.
    fn on_resets(&mut self) {
        let t = self.t;
        for i in 0..self.rows.len() {
            let before = self.fill_of(i);
            let filled = self.ctx.filled();
            let drops = self.rows[i].win(filled).reset.is_some_and(|r| r <= t);
            let mut reseen = false;
            for &w in self.ctx.modeled() {
                if self.rows[i].win(w).reset.is_none_or(|r| r > t) {
                    continue;
                }
                self.capture(i, w);
                let carrier = i == self.carrier && self.stop.is_none();
                let row = &mut self.rows[i];
                // Only the carrier's own window is followed past a reset: any other Mac may
                // have used any other row's.
                let keeps = row.seen;
                let win = row.win_mut(w);
                win.fills_at = None;
                win.low_fired = false;
                if !keeps {
                    win.known = Known::Unknown;
                    win.reset = None;
                    continue;
                }
                win.acc = 0;
                win.known = Known::Exact;
                win.reset = match w {
                    Window::Week => win.reset.map(|r| next_reset(r, w.len_s(), t)),
                    Window::Five => carrier.then(|| t + w.len_s()),
                };
                reseen |= row.seen;
            }
            if drops {
                self.push_knot(i, t, before);
                let after = self.fill_of(i);
                self.push_knot(i, t, after);
            }
            if reseen {
                self.read_into_state(i);
            }
        }
    }

    fn full_window(&self) -> Option<Window> {
        let row = &self.rows[self.carrier];
        let full = i128::from(FULL_X10) * row.scale();
        self.ctx
            .modeled()
            .iter()
            .copied()
            .find(|w| row.win(*w).reset.is_some() && row.win(*w).acc >= full)
    }

    fn limited(&self, label: &str) -> bool {
        self.ctx
            .roster
            .row(label)
            .map(|row| {
                let enrolled = self.ctx.enrolled.contains(label);
                matches!(
                    accounts::condition(row, &self.state, enrolled, self.t),
                    Condition::Limited { .. }
                )
            })
            .unwrap_or(false)
    }

    /// Opens the carrier's 5-hour window at first use, in the working pass.
    fn open_five(&mut self) {
        if self.ctx.kind != PassKind::Working {
            return;
        }
        let t = self.t;
        let row = &mut self.rows[self.carrier];
        if row.five.reset.is_none_or(|r| r <= t) && row.five.known != Known::Unknown {
            row.five = Win {
                reset: Some(t + Window::Five.len_s()),
                ..Win::empty(Known::Exact)
            };
        }
    }

    /// R6: the carrier walls on `w`.
    fn wall(&mut self, w: Window) -> Option<()> {
        let label = self.carrier_label();
        let reset = self.rows[self.carrier].win(w).reset;
        if self.stop.is_none() {
            self.close_link(Ends::Walls);
            let _ = accounts::note_wall(&mut self.state, &label, w.id(), reset, self.t, self.t);
        }
        self.at_wall(w)
    }

    /// The end of the carrier's own standing wall: `None` where none stands, and a stop is
    /// planned to the resume bound. The worst case's invented wall stands past the bound
    /// ([`Self::assume_worst`]), so a stop planned to it is still open there, as one planned to
    /// the bound is.
    fn own_wall(&self) -> Option<i64> {
        self.state
            .accounts
            .get(&self.carrier_label())?
            .walls
            .values()
            .filter_map(accounts::Wall::stands_until)
            .filter(|end| self.t < *end)
            .max()
    }

    /// R6's decision at the carrier's standing wall on `w`.
    fn at_wall(&mut self, w: Window) -> Option<()> {
        let point = Point::Wall {
            window: w.id().to_owned(),
            at: Some(self.t),
        };
        match self.advise(&point) {
            Advice::Move { to, rotates, .. } => {
                if !self.rotate(&to, rotates) {
                    // The rotation was refused (spent, raced, pinned): the work stays walled.
                    let until = self.own_wall().unwrap_or(self.ctx.resume_bound);
                    self.begin_stop(until, w);
                    return Some(());
                }
                let on = self.index(&to);
                self.end_stop(on);
                if self.t >= self.ctx.horizon {
                    // Past the horizon a move only ends the stop: no link follows.
                    return Some(());
                }
                self.take(&to, Ends::Walls)
            }
            Advice::Wait { until } | Advice::Stay(StayWhy::Breaker { until }) => {
                self.begin_stop(until, w);
                Some(())
            }
            Advice::Resume { .. } => {
                let on = Some(self.carrier);
                self.end_stop(on);
                Some(())
            }
            Advice::Stay(_) | Advice::Probe => {
                let until = self.own_wall().unwrap_or(self.ctx.resume_bound);
                self.begin_stop(until, w);
                Some(())
            }
        }
    }

    /// Plans the stop (or re-plans the open one) to `until`, unclamped: it may lie past the
    /// horizon. Each row whose real wall stands exactly to that instant is noted with the vendor
    /// reset it is the grace of ([`Planned::resets`]).
    fn begin_stop(&mut self, until: i64, w: Window) {
        let until = until.max(self.t + 1);
        let resets = self
            .rows
            .iter()
            .enumerate()
            .filter_map(|(i, row)| {
                let walls = &self.state.accounts.get(&row.label)?.walls;
                walls
                    .values()
                    .filter(|wall| wall.stands_until() == Some(until))
                    .find_map(|wall| {
                        wall.until
                            .filter(|reset| reset.saturating_add(accounts::RESET_GRACE_S) == until)
                    })
                    .map(|reset| (i, reset))
            })
            .collect();
        match &mut self.stop {
            Some(stop) => {
                stop.until = until;
                stop.resets = resets;
            }
            None => {
                self.stop = Some(Planned {
                    from: self.t,
                    until,
                    window: w,
                    resets,
                });
            }
        }
    }

    /// Ends the open stop now, the work going on on row `on`: [`StopEnd::Reset`] where that is
    /// the planned instant and `on`'s real wall ends there by its vendor reset, else
    /// [`StopEnd::About`] now. `None` is a stop nothing ended by the resume bound
    /// ([`StopEnd::Open`]). One that ends within [`STOP_GRACE_S`] of its start is no stop.
    fn end_stop(&mut self, on: Option<usize>) {
        let Some(stop) = self.stop.take() else {
            return;
        };
        self.link_from = self.t;
        let end = match on {
            None => StopEnd::Open,
            Some(_) if self.t.saturating_sub(stop.from) <= STOP_GRACE_S => return,
            Some(i) => stop
                .resets
                .iter()
                .find(|(row, _)| *row == i)
                .filter(|_| self.t == stop.until)
                .map_or(StopEnd::About(self.t), |&(_, reset)| StopEnd::Reset(reset)),
        };
        let credits = self.ctx.credits
            && self
                .state
                .accounts
                .get(&self.carrier_label())
                .is_none_or(|a| !matches!(a.credits, Some(accounts::Credits::Off(_))));
        self.stops.push(Stop {
            from: stop.from,
            end,
            window: stop.window,
            credits,
        });
    }

    /// R7's rotation onto `to`, where the advice `rotates`: the executor's compare-and-set, then
    /// the served response that proves it. `false` where the rotation was refused, with nothing
    /// changed.
    fn rotate(&mut self, to: &str, rotates: bool) -> bool {
        if self.index(to).is_none() {
            return false;
        }
        if !rotates {
            return true;
        }
        let from = self.carrier_label();
        let active = self.state.active.clone();
        let moved = accounts::begin_rotation(
            &mut self.state,
            self.ctx.roster,
            self.ctx.enrolled,
            active.as_deref(),
            Mover {
                sid: "outlook",
                account: &from,
            },
            to,
            self.t,
        );
        if moved.is_err() {
            return false;
        }
        let _ = accounts::prove(&mut self.state, to, self.t);
        true
    }

    /// R7: the work moves to `to`, rotated already; `None` where it forked at a lean.
    fn take(&mut self, to: &str, ends: Ends) -> Option<()> {
        let Some(b) = self.index(to) else {
            return Some(());
        };
        if ends == Ends::Leaves {
            self.close_link(Ends::Leaves);
        } else {
            self.link_from = self.t;
        }
        self.carrier = b;
        let known = self.rows[b].week.known;
        if known == Known::Exact || self.rows[b].seen {
            self.rows[b].seen = true;
            self.open_five();
            self.note_fill(b);
            // A row the policy took on a figure it holds of it, though its truth is spent (a
            // wall the clone no longer stands, its window not yet reset): it walls at once.
            if let Some(w) = self.full_window().filter(|_| self.stop.is_none()) {
                return self.wall(w);
            }
            self.read_into_state(b);
            return Some(());
        }
        match self.branch {
            Branch::Main => {
                self.fork(b);
                None
            }
            Branch::Worst => {
                self.assume_worst(b);
                Some(())
            }
            Branch::Best => {
                self.assume_best(b);
                Some(())
            }
        }
    }

    /// The worst case of an unread row: it walls at once, out to the resume bound. Its reset and
    /// its wall's end (the bound plus the reset grace) lie at or past the bound, which no event
    /// reaches ([`Self::next_event`]): the invention is never an event and never ends a stop,
    /// by construction. Its fill is an assumption, not a figure: the week is
    /// [`Known::Unknown`], so [`Pass::fills`] never draws it full (never an inferred 100%).
    fn assume_worst(&mut self, b: usize) {
        let label = self.rows[b].label.clone();
        let bound = self.ctx.resume_bound;
        // It walls at its first turn: a link of no length, so the chain names it.
        self.close_link(Ends::Walls);
        self.note_fill(b);
        let row = &mut self.rows[b];
        row.seen = true;
        let full = i128::from(FULL_X10) * row.scale();
        row.week = Win {
            acc: full,
            reset: Some(bound),
            low_fired: true,
            fills_at: Some(self.t),
            ..Win::empty(Known::Unknown)
        };
        self.note_fill(b);
        let _ = accounts::note_wall(
            &mut self.state,
            &label,
            accounts::WEEK_LIMIT,
            Some(bound),
            self.t,
            self.t,
        );
        // The policy believes a vendor reset only within `RESET_BELIEVED_S` of its wall
        // (`accounts::believed_reset`), so the wall was noted with none; the worst case's
        // invention stands to the resume bound.
        if let Some(wall) = self
            .state
            .accounts
            .get_mut(&label)
            .and_then(|a| a.walls.get_mut(accounts::WEEK_LIMIT))
        {
            wall.until = Some(bound);
        }
        let _ = self.at_wall(Window::Week);
    }

    fn assume_best(&mut self, b: usize) {
        let t = self.t;
        let row = &mut self.rows[b];
        row.seen = true;
        match (row.week.known, row.read_week) {
            (Known::Lower, Some(seen)) => {
                row.week.known = Known::Exact;
                row.week.acc = i128::from(seen.pct_x10) * row.scale();
                row.week.reset = Some(seen.reset);
            }
            _ => {
                row.week = Win {
                    reset: Some(t + Window::Week.len_s()),
                    ..Win::empty(Known::Exact)
                };
            }
        }
        if row.five.known != Known::Exact {
            row.five = Win::empty(Known::Exact);
        }
        self.open_five();
        self.read_into_state(b);
        self.note_fill(b);
    }

    fn fork(&mut self, b: usize) {
        let label = self.rows[b].label.clone();
        let why = self.rows[b].fresh_week;
        let mut worst = self.clone();
        worst.branch = Branch::Worst;
        worst.chain.clear();
        worst.stops.clear();
        worst.link_from = self.t;
        worst.seed_fills();
        worst.assume_worst(b);
        let mut best = self.clone();
        best.branch = Branch::Best;
        best.chain.clear();
        best.stops.clear();
        best.link_from = self.t;
        best.seed_fills();
        best.assume_best(b);
        let at = self.t;
        self.lean = Some(Box::new(Lean {
            at,
            row: label,
            why,
            worst: worst.run(),
            best: best.run(),
        }));
    }

    /// Runs the pass to its horizon, and a stop open there on to its end (at most the resume
    /// bound).
    fn run(mut self) -> Pass {
        if self.fills.is_empty() {
            self.seed_fills();
        }
        self.open_five();
        if let Some(w) = self.full_window().filter(|_| self.stop.is_none()) {
            if self.wall(w).is_none() {
                return self.finish(true);
            }
        } else if self.stop.is_none() && self.branch == Branch::Main {
            // Now is a leave point too: a carrier already low is left at the next turn's end.
            let c = self.carrier;
            for &w in self.ctx.modeled() {
                let row = &self.rows[c];
                if row.win(w).acc >= i128::from(w.low_x10()) * row.scale() {
                    self.rows[c].win_mut(w).low_fired = true;
                }
            }
            self.read_into_state(c);
            if let Advice::Move { to, rotates, .. } = self.advise(&Point::TurnEnd)
                && self.rotate(&to, rotates)
                && self.take(&to, Ends::Leaves).is_none()
            {
                return self.finish(true);
            }
        }
        loop {
            if self.events >= MAX_EVENTS {
                self.cut_at = Some(self.t);
                break;
            }
            self.events += 1;
            let next = self.next_event();
            debug_assert!(next >= self.t, "the replay never steps back");
            self.advance(next);
            self.t = next;
            if self.t >= self.ctx.horizon {
                self.close_fills();
                // Past the horizon the replay only ends an open stop.
                if self.stop.is_none() || self.t >= self.ctx.resume_bound {
                    break;
                }
            }
            self.on_resets();
            if let Some(Planned { until, window, .. }) = self.stop {
                if self.t < until {
                    continue;
                }
                if self.limited(&self.carrier_label()) {
                    if self.at_wall(window).is_none() {
                        return self.finish(true);
                    }
                    if self.stop.is_some() {
                        continue;
                    }
                } else {
                    let on = Some(self.carrier);
                    self.end_stop(on);
                }
                if self.t >= self.ctx.horizon {
                    break;
                }
                if self.limited(&self.carrier_label()) {
                    continue;
                }
                self.open_five();
                let c = self.carrier;
                self.read_into_state(c);
                continue;
            }
            if let Some(w) = self.full_window() {
                if self.wall(w).is_none() {
                    return self.finish(true);
                }
                continue;
            }
            let c = self.carrier;
            for &w in self.ctx.modeled() {
                let row = &self.rows[c];
                let win = row.win(w);
                if win.acc >= i128::from(w.low_x10()) * row.scale() {
                    self.rows[c].win_mut(w).low_fired = true;
                }
            }
            self.read_into_state(c);
            if let Advice::Move { to, rotates, .. } = self.advise(&Point::TurnEnd)
                && self.rotate(&to, rotates)
                && self.take(&to, Ends::Leaves).is_none()
            {
                return self.finish(true);
            }
        }
        self.finish(false)
    }

    fn finish(mut self, forked: bool) -> Pass {
        if !forked {
            if self.stop.is_some() {
                // Nothing ended it by the resume bound (or the event bound cut it).
                self.close_fills();
                self.end_stop(None);
            } else if self.link_from < self.ctx.horizon {
                self.t = self.t.min(self.ctx.horizon);
                self.close_link(Ends::Horizon);
            }
            for i in 0..self.rows.len() {
                for &w in self.ctx.modeled() {
                    self.capture(i, w);
                }
            }
        }
        self.close_fills();
        Pass {
            kind: self.ctx.kind,
            horizon: self.ctx.horizon,
            chain: self.chain,
            stops: self.stops,
            lean: self.lean,
            cut_at: self.cut_at,
            events: self.events,
            proj: self.proj,
            fills: self.fills,
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The builder

/// Each row of `roster`'s plan as seen: its `series`' newest `plan` row, else its renewal's word
/// (`renewals`, [`renewals_of`]), with the multiple it names ([`Plan::named`]); [`Plan::m`] is 1,
/// and the outlook's builder fills it in. A row neither names has none.
#[must_use]
pub fn plans_seen(
    roster: &Roster,
    series: &BTreeMap<String, Vec<SeriesRow>>,
    renewals: &BTreeMap<String, String>,
) -> BTreeMap<String, Plan> {
    let mut out = BTreeMap::new();
    for row in &roster.rows {
        let said = series
            .get(&row.label)
            .and_then(|rows| {
                let plan = account_series::newest_plan(rows)?;
                let at = rows
                    .iter()
                    .rev()
                    .find(|r| matches!(&r.said, account_series::Said::Plan(p) if p == plan))
                    .map(|r| r.t);
                Some(Plan {
                    m: 1,
                    named: multiple_of(&plan.word, plan.tier.as_deref()),
                    word: Some(plan.word.clone()),
                    tier: plan.tier.clone(),
                    source: PlanSource::Profile,
                    at,
                })
            })
            .or_else(|| {
                let word = renewals.get(&row.label)?.to_ascii_lowercase();
                Some(Plan {
                    m: 1,
                    named: multiple_of(&word, None),
                    word: Some(word),
                    tier: None,
                    source: PlanSource::Renewal,
                    at: None,
                })
            });
        if let Some(plan) = said {
            out.insert(row.label.clone(), plan);
        }
    }
    out
}

fn account_access(
    row: &accounts::Row,
    state: &State,
    enrolled: &BTreeSet<String>,
    now: i64,
) -> bool {
    let enrolled = enrolled.contains(&row.label);
    !matches!(
        accounts::condition(row, state, enrolled, now),
        Condition::SignedOut | Condition::NotEnrolled
    ) && accounts::row_available_here(row, state, now)
}

/// Settle only a canary whose account was already available to this replay.
/// Quota simulation cannot supply a missing login or override a retained access refusal.
fn prove_available_active(inputs: &Inputs<'_>, state: &mut State) {
    let Some(active) = state.active.clone() else {
        return;
    };
    if inputs
        .roster
        .row(&active)
        .is_some_and(|row| account_access(row, state, inputs.enrolled, inputs.now))
    {
        let _ = accounts::prove(state, &active, inputs.now);
    }
}

fn carrier_of<'a>(inputs: &Inputs<'a>) -> Result<&'a accounts::Row, Refusal> {
    let roster = inputs.roster;
    let active = inputs
        .state
        .active
        .as_deref()
        .and_then(|label| roster.row(label));
    // A cold launch choice is not permission to move existing work while rotation is off.
    if inputs.mode == Mode::Off && inputs.state.active.is_some() {
        return active
            .filter(|row| account_access(row, inputs.state, inputs.enrolled, inputs.now))
            .ok_or(Refusal::RotationOff);
    }
    if let Some(row) = active {
        let access = account_access(row, inputs.state, inputs.enrolled, inputs.now);
        let enrolled = inputs.enrolled.contains(&row.label);
        if inputs.state.pinned
            && row.held_by.is_none()
            && accounts::condition(row, inputs.state, enrolled, inputs.now).runs()
            && !access
        {
            return Err(Refusal::AccountUnavailable);
        }
    }
    let active =
        active.filter(|row| account_access(row, inputs.state, inputs.enrolled, inputs.now));
    if let Some(row) = active {
        let enrolled = inputs.enrolled.contains(&row.label);
        if accounts::condition(row, inputs.state, enrolled, inputs.now).runs() {
            return Ok(row);
        }
    }
    accounts::launch_row(roster, inputs.state, inputs.enrolled, inputs.now)
        .or(active)
        .or_else(|| {
            roster
                .rows
                .iter()
                .find(|row| account_access(row, inputs.state, inputs.enrolled, inputs.now))
        })
        .ok_or(if roster.rows.is_empty() {
            Refusal::NoFigure
        } else {
            Refusal::AccountUnavailable
        })
}

fn win_of(seen: Option<Seen>, fresh: Fresh, m: u32, now: i64, w: Window) -> Win {
    let scale = i128::from(m.max(1)) * i128::from(DAY);
    let known = fresh.known();
    let admitted = seen.filter(|s| super::profile::usage_read_age(s.at, now).is_some());
    let current = admitted.filter(|s| s.reset > now);
    match (fresh, current) {
        (Fresh::Walled, s) => Win {
            acc: i128::from(FULL_X10) * scale,
            reset: s.map(|s| s.reset),
            low_fired: true,
            fills_at: Some(now),
            ..Win::empty(Known::Exact)
        },
        (_, Some(s)) => Win {
            acc: i128::from(s.pct_x10) * scale,
            reset: Some(s.reset),
            ..Win::empty(known)
        },
        (_, None) if w == Window::Five && admitted.is_some() => Win::empty(Known::Exact),
        (_, None) => Win::empty(Known::Unknown),
    }
}

/// Only the replay's private copy withdraws clock-invalid quota inputs. Retain
/// policy walls, access, claims, pins and session facts; the source stays lossless.
fn quota_replay_state(source: &State, now: i64) -> State {
    let mut replay = source.clone();
    for account in replay.accounts.values_mut() {
        let barriers = account.windows.clone();
        let admitted = |window: &str, at: i64| {
            super::profile::usage_read_age(at, now).is_some()
                && barriers
                    .get(window)
                    .is_none_or(|barrier| barrier.admits_reading(at))
        };
        account.windows.retain(|_, figure| {
            super::profile::usage_read_age(figure.at, now).is_some() || figure.cut()
        });
        for sample in [&mut account.sample, &mut account.before]
            .into_iter()
            .flatten()
        {
            if !admitted(
                accounts::WEEK_LIMIT,
                sample.week_read_at.unwrap_or(sample.at),
            ) {
                sample.week_pct = None;
                sample.week_reset = None;
            }
            if !admitted(
                accounts::SESSION_LIMIT,
                sample.session_read_at.unwrap_or(sample.at),
            ) {
                sample.session_pct = None;
                sample.session_reset = None;
            }
        }
        account.warned_week = account
            .warned_week
            .take()
            .filter(|warning| admitted(accounts::WEEK_LIMIT, warning.at));
        account.warned_session = account
            .warned_session
            .take()
            .filter(|warning| admitted(accounts::SESSION_LIMIT, warning.at));
    }
    replay
}

/// One decision the replay took: the clone it asked on, the carrier, the point, the instant and
/// the policy's answer ([`outlook_traced`]).
#[derive(Clone, Debug)]
pub struct Decision {
    /// The clone as the policy read it.
    pub state: State,
    /// The carrier it asked for.
    pub carrier: String,
    /// The point.
    pub point: Point,
    /// The instant.
    pub t: i64,
    /// The pass's horizon: a decision at or past it only ends a stop.
    pub horizon: i64,
    /// The answer.
    pub advice: Advice,
    /// The carrier's modeled figures then (each window, its tenths used, its reset), which the
    /// clone must hold at a turn's end: the policy decides on the replay's own figures.
    pub modeled: Vec<(Window, u32, i64)>,
}

/// Builds the outlook from `inputs` ([`Inputs`]): reads nothing else, writes nothing, probes
/// nothing; every function it calls on the state runs on its own clone.
#[must_use]
pub fn outlook(inputs: &Inputs<'_>) -> Outlook {
    build(inputs, None)
}

/// Each roster row's renewal `subscriptionType`, where its login's binding holds one
/// ([`Inputs::renewals`]).
#[must_use]
pub fn renewals_of(roster: &Roster, logins: &accounts::Logins) -> BTreeMap<String, String> {
    roster
        .rows
        .iter()
        .filter_map(|r| Some((r.label.clone(), logins.get(&r.label)?.subscription.clone()?)))
        .collect()
}

/// Each roster row's series on `machine` under `harness_state` as the outlook reads it at `now`
/// ([`account_series::outlook_view`], what the card's [`account_series::Tail`] holds), rows with
/// none left out ([`Inputs::series`]).
#[must_use]
pub fn series_of(
    harness_state: &std::path::Path,
    roster: &Roster,
    machine: &str,
    now: i64,
) -> BTreeMap<String, Vec<SeriesRow>> {
    roster
        .rows
        .iter()
        .map(|r| {
            (
                r.label.clone(),
                account_series::outlook_view(
                    account_series::read(harness_state, &r.label, machine),
                    now,
                ),
            )
        })
        .filter(|(_, rows)| !rows.is_empty())
        .collect()
}

/// The mode the outlook replays under for `policy`: `[harness] accounts` where the harness runs
/// Claude Code, else [`Mode::Off`], since nothing then rotates its accounts. The window's switch
/// also folds `[harness] headless` in a headless instance (`harness_host`), which a report run
/// outside the window cannot see.
#[must_use]
pub fn replay_mode(policy: &crate::supervise::SupervisorConfig) -> Mode {
    if policy.on_for(aterm_phase::Program::Claude) {
        policy.accounts
    } else {
        Mode::Off
    }
}

/// The outlook `aterm harness accounts` reports: the series and the renewals read from
/// `harness_state` and `logins` beside the state the report read, nothing written.
#[must_use]
pub fn for_report(
    harness_state: &std::path::Path,
    (roster, state, logins, enrolled): (&Roster, &State, &accounts::Logins, &BTreeSet<String>),
    (mode, credits): (Mode, bool),
    (now, offset): (i64, fn(i64) -> i64),
) -> Outlook {
    let series = series_of(harness_state, roster, &state.machine, now);
    let renewals = renewals_of(roster, logins);
    outlook(&Inputs {
        roster,
        state,
        enrolled,
        series: &series,
        renewals: &renewals,
        mode,
        credits,
        now,
        offset,
    })
}

/// [`outlook`] with every decision its replay took, in order, for a test to hold against the
/// policy ([`Decision`]).
#[must_use]
pub fn outlook_traced(inputs: &Inputs<'_>) -> (Outlook, Vec<Decision>) {
    let trace = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let out = build(inputs, Some(trace.clone()));
    let decisions = trace.borrow().clone();
    (out, decisions)
}

fn build(
    inputs: &Inputs<'_>,
    trace: Option<std::rc::Rc<std::cell::RefCell<Vec<Decision>>>>,
) -> Outlook {
    let now = inputs.now;
    let state = inputs.state;
    let roster = inputs.roster;
    let plans = plans_seen(inputs.roster, inputs.series, inputs.renewals);
    let choice = carrier_of(inputs);
    let carrier = choice.ok();
    let carrier_label = carrier.map(|r| r.label.clone());
    let carrier_plan = carrier_label
        .as_ref()
        .and_then(|label| plans.get(label))
        .cloned();
    // §5.1: a row whose plan names no multiple counts as the carrier's; where the carrier's
    // names none either, every row counts as 1, so a common value cancels out.
    let carrier_m = carrier_plan.as_ref().and_then(|p| p.named);
    let default_m = carrier_m.unwrap_or(1);
    let mut approx = Vec::new();
    if let Some(carrier) = carrier_label
        .as_ref()
        .filter(|_| carrier_m.is_none() && roster.rows.len() > 1)
    {
        approx.push(Approx::PlansAlike {
            carrier: carrier.clone(),
        });
    }
    let mut rows = Vec::new();
    let mut sim_rows = Vec::new();
    for row in &roster.rows {
        let enrolled = inputs.enrolled.contains(&row.label);
        let cond = accounts::condition(row, state, enrolled, now);
        let is_carrier = carrier_label.as_deref() == Some(row.label.as_str());
        let mut plan = plans.get(&row.label).cloned().unwrap_or(Plan {
            m: 1,
            named: None,
            word: None,
            tier: None,
            source: PlanSource::Assumed,
            at: None,
        });
        plan.m = match (carrier_m, plan.named) {
            (Some(_), Some(m)) => m,
            (Some(c), None) => {
                if !is_carrier && let Some(carrier) = &carrier_label {
                    approx.push(Approx::PlanAssumed {
                        label: row.label.clone(),
                        word: plan.word.clone(),
                        like: carrier_plan.as_ref().and_then(Plan::title),
                        carrier: carrier.clone(),
                    });
                }
                c
            }
            (None, _) => 1,
        };
        let (fresh_week, seen_week) = fresh_of(row, state, cond, is_carrier, Window::Week, now);
        let (fresh_five, seen_five) = fresh_of(row, state, cond, is_carrier, Window::Five, now);
        let window = |w: Window, fresh: Fresh, seen: Option<Seen>| WindowOutlook {
            window: w,
            used_x10: seen
                .filter(|s| {
                    fresh.shows_figure() && super::profile::usage_read_age(s.at, now).is_some()
                })
                .map(|s| s.pct_x10),
            seen_x10: seen.map(|s| s.pct_x10),
            reset: seen.map(|s| s.reset),
            at: seen.map(|s| s.at),
            fresh,
            projection: None,
            source: seen.map(|s| s.source),
        };
        let walled = matches!(cond, Condition::Limited { .. }).then(|| {
            let mut clone = quota_replay_state(state, now);
            prove_available_active(inputs, &mut clone);
            let window = state
                .accounts
                .get(&row.label)
                .and_then(|a| {
                    a.walls
                        .iter()
                        .filter(|(_, w)| w.stands_until().is_some_and(|end| end > now))
                        .map(|(k, _)| k.clone())
                        .next()
                })
                .unwrap_or_else(|| accounts::WEEK_LIMIT.to_owned());
            let (to, until) = match accounts::advise_past_probe(
                roster,
                &clone,
                inputs.enrolled,
                &me(&row.label),
                &Point::Wall {
                    window,
                    at: Some(now),
                },
                inputs.mode,
                now,
            ) {
                Advice::Move { to, .. } => (Some(to), None),
                Advice::Wait { until } => (None, Some(until)),
                _ => (
                    None,
                    state
                        .accounts
                        .get(&row.label)
                        .and_then(|a| a.limited_until(now)),
                ),
            };
            Walled {
                to,
                until,
                reset: until.and_then(|u| wall_reset(state, u)),
            }
        });
        rows.push(RowOutlook {
            label: row.label.clone(),
            class: row.class,
            plan: plan.clone(),
            active: state.active.as_deref() == Some(row.label.as_str()),
            carrier: is_carrier,
            condition: cond,
            ran_at: state.accounts.get(&row.label).and_then(|a| a.ran_at),
            windows: vec![
                window(Window::Week, fresh_week, seen_week),
                window(Window::Five, fresh_five, seen_five),
            ],
            walled,
            read: inputs
                .series
                .get(&row.label)
                .map(|rows| reads_of(rows, now))
                .unwrap_or_default(),
        });
        // The carrier is followed from its reading whatever its order here (the replay holds the
        // work on it, as R7 follows a row once taken); every other row read before now is a
        // bound, since any Mac may run any account.
        let start = |seen, fresh, w| {
            let mut win = win_of(seen, fresh, plan.m, now, w);
            if is_carrier && win.known == Known::Lower {
                win.known = Known::Exact;
            }
            win
        };
        sim_rows.push(SimRow {
            label: row.label.clone(),
            m: plan.m,
            seen: is_carrier,
            week: start(seen_week, fresh_week, Window::Week),
            five: start(seen_five, fresh_five, Window::Five),
            fresh_week,
            read_week: seen_week
                .filter(|s| s.reset > now && super::profile::usage_read_age(s.at, now).is_some()),
        });
    }
    let multiples: BTreeMap<String, u32> =
        rows.iter().map(|r| (r.label.clone(), r.plan.m)).collect();
    let counted: BTreeSet<u32> = rows
        .iter()
        .filter(|r| r.condition.runs() || r.carrier)
        .map(|r| r.plan.m)
        .collect();
    if counted.len() > 1 {
        approx.push(Approx::MultiplesScaled);
    }
    // The pace is read off the row the work ran on: the store's active row, where the roster
    // lists it, though it cannot carry the work now (a wall, a sign-out), else the carrier.
    let pacer = rows
        .iter()
        .find(|r| r.active)
        .or_else(|| rows.iter().find(|r| r.carrier));
    let demand = demand_of(inputs, &multiples, default_m, pacer);
    let mut out = Outlook {
        as_of: now,
        offsets: Offsets::around(now, inputs.offset),
        mode: inputs.mode,
        credits: inputs.credits,
        carrier: carrier_label.clone(),
        demand,
        rows,
        week: None,
        working: None,
        verdict: Verdict::Continuous,
        approx,
        not_needed: Vec::new(),
    };
    let Some(carrier_label) = carrier_label else {
        out.verdict = Verdict::Refused(choice.err().unwrap_or(Refusal::NoFigure));
        return out;
    };
    let Some(carrier_index) = sim_rows.iter().position(|r| r.label == carrier_label) else {
        out.verdict = Verdict::Refused(Refusal::NoFigure);
        return out;
    };
    let walled_now = out.rows.get(carrier_index).and_then(|r| r.walled.clone());
    let week_pace = match demand.week {
        Ok(pace) => pace,
        Err(why) => {
            out.verdict = walled_now.map_or(Verdict::Refused(why), |w| Verdict::Walled {
                to: w.to,
                until: w.until,
            });
            return out;
        }
    };
    let mut clone = quota_replay_state(state, now);
    if clone.active.is_none() {
        clone.active = Some(carrier_label.clone());
    }
    prove_available_active(inputs, &mut clone);
    // The carrier's figure advanced from its reading's instant to now, at the week's pace,
    // where the work ran on it since (a reading of now or as of the last hour, §6): a row the
    // work only now moves onto (idle here since its reading) holds its reading.
    {
        let row = &mut sim_rows[carrier_index];
        for (w, seen, rate) in [
            (
                Window::Week,
                out.rows[carrier_index].window(Window::Week),
                Some(week_pace.x10_m_per_day),
            ),
            (
                Window::Five,
                out.rows[carrier_index].window(Window::Five),
                demand.five.ok().map(|p| p.x10_m_per_day),
            ),
        ] {
            let (Some(at), Some(rate)) = (
                seen.filter(|s| matches!(s.fresh, Fresh::Now | Fresh::AsOf))
                    .and_then(|s| s.at),
                rate,
            ) else {
                continue;
            };
            let win = row.win_mut(w);
            if win.known == Known::Exact && win.reset.is_some_and(|r| r > now) {
                let full = i128::from(FULL_X10) * i128::from(row.m.max(1)) * i128::from(DAY);
                let win = row.win_mut(w);
                win.acc = (win.acc + i128::from(rate) * i128::from((now - at).max(0))).min(full);
            }
        }
    }
    let sim = |kind: PassKind, horizon: i64, week_rate: i64, five_rate: Option<i64>| Sim {
        ctx: Ctx {
            roster,
            enrolled: inputs.enrolled,
            mode: inputs.mode,
            credits: inputs.credits,
            kind,
            horizon,
            resume_bound: horizon.saturating_add(accounts::WEEK_WINDOW_S),
            week_rate,
            five_rate,
        },
        state: clone.clone(),
        rows: sim_rows.clone(),
        carrier: carrier_index,
        t: now,
        link_from: now,
        stop: None,
        chain: Vec::new(),
        stops: Vec::new(),
        proj: BTreeMap::new(),
        fills: BTreeMap::new(),
        knots: 0,
        fills_closed: false,
        events: 0,
        cut_at: None,
        branch: Branch::Main,
        lean: None,
        trace: trace.clone(),
    };
    let week = sim(
        PassKind::Week,
        now + accounts::WEEK_WINDOW_S,
        week_pace.x10_m_per_day,
        None,
    )
    .run();
    let working = demand.five.ok().map(|five| {
        sim(
            PassKind::Working,
            now + WORKING_HORIZON_S,
            demand.week_working.unwrap_or(week_pace.x10_m_per_day),
            Some(five.x10_m_per_day),
        )
        .run()
    });
    let mut used = BTreeSet::new();
    week.used(&mut used);
    if let Some(working) = &working {
        working.used(&mut used);
    }
    let week_proj = week.projections();
    let working_proj = working.as_ref().map(Pass::projections).unwrap_or_default();
    for row in &mut out.rows {
        for win in &mut row.windows {
            let key = (row.label.clone(), win.window);
            win.projection = match win.window {
                Window::Week => week_proj.get(&key).copied(),
                Window::Five => working_proj.get(&key).copied(),
            }
            .filter(|_| win.fresh != Fresh::ClockInvalid);
        }
    }
    out.not_needed = out
        .rows
        .iter()
        .filter(|r| !used.contains(&r.label) && r.condition.runs())
        .map(|r| r.label.clone())
        .collect();
    out.verdict = verdict_of(walled_now, &week, working.as_ref());
    if inputs.credits
        && let Verdict::Stop { from, .. } = out.verdict
        && state
            .accounts
            .get(&carrier_label)
            .is_none_or(|a| !matches!(a.credits, Some(accounts::Credits::Off(_))))
    {
        out.verdict = Verdict::Credits { at: from };
    }
    out.week = Some(week);
    out.working = working;
    out
}

/// A row's week as this Mac read it ([`RowOutlook::read`]): each admitted reading of the last
/// [`READ_SPAN_S`] (a beat repeats the reading before it), the last of each hour of each window
/// kept, oldest first, at most the newest [`READ_MAX`].
fn reads_of(rows: &[SeriesRow], now: i64) -> Vec<Read> {
    let mut figs: Vec<Read> = Vec::new();
    let mut last: Option<Read> = None;
    for row in rows {
        let read = match &row.said {
            // A beat repeats the last `fig` row's figures, both windows: `last` is that row's
            // week, `None` included, so a beat after a row that read no week repeats none.
            account_series::Said::Fig { seven_day, .. } => {
                last = seven_day.as_ref().map(|fig| Read {
                    t: fig.at,
                    x10: u16::try_from(fig.pct_x10.min(1000)).unwrap_or(1000),
                    reset: fig.reset,
                    epoch: fig.epoch,
                });
                last
            }
            account_series::Said::Beat => last.map(|prev| Read { t: row.t, ..prev }),
            account_series::Said::Plan(_)
            | account_series::Said::Close(_)
            | account_series::Said::Day(_)
            | account_series::Said::Event(_) => None,
        };
        let Some(read) = read else {
            continue;
        };
        if super::profile::usage_read_age(read.t, now).is_some() && read.t >= now - READ_SPAN_S {
            figs.push(Read {
                t: read.t.min(now),
                ..read
            });
        }
    }
    figs.sort_by_key(|r| r.t);
    let mut out: Vec<Read> = Vec::with_capacity(figs.len().min(READ_MAX));
    for read in figs {
        match out.last_mut() {
            Some(prev)
                if prev.t.div_euclid(3600) == read.t.div_euclid(3600)
                    && (prev.reset, prev.epoch) == (read.reset, read.epoch) =>
            {
                *prev = read;
            }
            _ => out.push(read),
        }
    }
    let cut = out.len().saturating_sub(READ_MAX);
    out.drain(..cut);
    out
}

/// The work's demand ([`Demand`]), its refusals and fallbacks read off `carrier`: the row the
/// work ran on (the store's active row, else the carrier the replay starts on).
fn demand_of(
    inputs: &Inputs<'_>,
    multiples: &BTreeMap<String, u32>,
    default_m: u32,
    carrier: Option<&RowOutlook>,
) -> Demand {
    let now = inputs.now;
    let refused = |w: Window| -> Option<Refusal> {
        let c = carrier?;
        let win = c.window(w)?;
        let at = win.at;
        if win.fresh == Fresh::Stale {
            return Some(Refusal::Stale {
                age_s: at.map_or(0, |at| now - at),
            });
        }
        if at.is_some_and(|at| {
            at > 0 && now > 0 && super::profile::usage_read_age(at, now).is_none()
        }) {
            return Some(Refusal::Skew);
        }
        if win.fresh == Fresh::ClockInvalid {
            return Some(Refusal::NoFigure);
        }
        if win.reset.is_none_or(|r| r <= now) || at.is_none() {
            return Some(Refusal::NoFigure);
        }
        None
    };
    let seen = |w: Window| {
        let win = carrier?.window(w)?;
        Some(Seen {
            pct_x10: win
                .used_x10
                .or_else(|| (win.fresh == Fresh::Walled).then_some(1000))?,
            reset: win.reset?,
            at: win.at?,
            source: win.source?,
        })
    };
    let carrier_m = carrier.map_or(default_m, |c| c.plan.m);
    let week = match carrier {
        None => Err(Refusal::NoFigure),
        Some(_) => match refused(Window::Week) {
            Some(why) => Err(why),
            None => pace_of(
                inputs.series,
                multiples,
                carrier_m,
                seen(Window::Week),
                Window::Week,
                (WEEK_LOOKBACK_S, WEEK_LEAST_S, WEEK_FALLBACK_AGE_S),
                Basis::Day,
                now,
            ),
        },
    };
    let five = match carrier {
        None => Err(Refusal::NoFigure),
        Some(c) => {
            let read_lately = c
                .window(Window::Five)
                .and_then(|w| w.at)
                .is_some_and(|at| now.saturating_sub(at) <= HOUR);
            match refused(Window::Five) {
                _ if !read_lately => Err(Refusal::Idle),
                Some(why) => Err(why),
                None => pace_of(
                    inputs.series,
                    multiples,
                    carrier_m,
                    seen(Window::Five),
                    Window::Five,
                    (FIVE_LOOKBACK_S, FIVE_LEAST_S, FIVE_FALLBACK_AGE_S),
                    Basis::Hour,
                    now,
                ),
            }
        }
    };
    let week_working = five.ok().and_then(|_| {
        let (units, earliest, before) =
            rise(inputs.series, multiples, carrier_m, Window::Week, HOUR, now);
        let span = if before {
            HOUR
        } else {
            earliest.map_or(0, |e| now - e)
        };
        (units >= WORKING_WEEK_MIN_X10 * i64::from(carrier_m.max(1)) && span >= FIVE_LEAST_S)
            .then(|| units * DAY / span)
    });
    Demand {
        week,
        five,
        week_working,
    }
}

/// A lean's worst case's first stop, its own lean's followed: [`Verdict::Lean`]'s `stop`.
pub(crate) fn worst_stop(lean: &Lean) -> Option<&Stop> {
    lean.worst
        .first_stop()
        .or_else(|| lean.worst.lean.as_ref().and_then(|l| worst_stop(l)))
}

/// Whether a lean's best case stops too, so the stop stands whatever the row holds.
pub(crate) fn best_stops(lean: &Lean) -> bool {
    lean.best.first_stop().is_some()
}

fn verdict_of(walled: Option<Walled>, week: &Pass, working: Option<&Pass>) -> Verdict {
    if let Some(w) = walled {
        return Verdict::Walled {
            to: w.to,
            until: w.until,
        };
    }
    let stop = |pass: &Pass, lean: Option<String>, s: &Stop| Verdict::Stop {
        pass: pass.kind,
        from: s.from,
        end: s.end,
        lean,
    };
    let mut leaning: Option<Verdict> = None;
    for pass in working.into_iter().chain(std::iter::once(week)) {
        if let Some(s) = pass.first_stop() {
            return stop(pass, None, s);
        }
        if let Some(lean) = &pass.lean
            && let Some(s) = worst_stop(lean)
        {
            if best_stops(lean) {
                return stop(pass, Some(lean.row.clone()), s);
            }
            if leaning.is_none() {
                leaning = Some(Verdict::Lean {
                    until: lean.at,
                    row: lean.row.clone(),
                    why: lean.why,
                    stop: Some((s.from, s.end)),
                });
            }
        }
    }
    leaning.unwrap_or(Verdict::Continuous)
}

// ---------------------------------------------------------------------------------------------
// Words: time, spans, rounding

const WEEKDAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
const WEEKDAYS_LONG: [&str; 7] = [
    "Thursday",
    "Friday",
    "Saturday",
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
];
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];
const MONTHS_LONG: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

/// A local instant's parts: days since the epoch, minutes into the day.
fn local(t: i64, offset_s: i64) -> (i64, i64) {
    let l = t + offset_s;
    (l.div_euclid(DAY), l.rem_euclid(DAY) / 60)
}

fn clock_of(minutes: i64) -> String {
    format!("{:02}:{:02}", minutes / 60, minutes % 60)
}

/// The glass's time words for `t` seen at `now`: `20:10` on the same local day, `Wed 22:30`
/// one to six days on, `Thu 12 Nov 05:00` from seven days (a day-7 instant shares today's
/// weekday, so it carries its date) and for any earlier day.
#[must_use]
pub fn when(t: i64, now: i64, offsets: &Offsets) -> String {
    when_in(t, now, offsets, &WEEKDAYS, &MONTHS)
}

/// The length of the [`when`] words at the head of `text`, in bytes (each an ASCII cell): `HH:MM`,
/// `Ddd HH:MM` or `Ddd D Mon HH:MM`, with an hour under 24, a minute under 60, a day of the month
/// from 1 to 31 with no leading zero, and [`when`]'s own weekday and month names; `None` where
/// `text` does not start with one. What follows is the caller's to judge.
#[must_use]
pub fn when_len(text: &str) -> Option<usize> {
    let clock = |s: &[u8]| {
        let two = |hi: u8, lo: u8, most: u8| {
            hi.is_ascii_digit() && lo.is_ascii_digit() && (hi - b'0') * 10 + (lo - b'0') <= most
        };
        matches!(s, [h1, h2, b':', m1, m2, ..] if two(*h1, *h2, 23) && two(*m1, *m2, 59))
    };
    let bytes = text.as_bytes();
    if clock(bytes) {
        return Some(5);
    }
    WEEKDAYS.iter().find(|day| text.starts_with(*day))?;
    let rest = bytes.get(3..)?.strip_prefix(b" ")?;
    if clock(rest) {
        return Some(4 + 5);
    }
    let digits = rest.iter().take_while(|b| b.is_ascii_digit()).count();
    let date = std::str::from_utf8(rest.get(..digits)?).ok()?;
    if !(1..=2).contains(&digits) || date.starts_with('0') || date.parse::<u8>().ok()? > 31 {
        return None;
    }
    let rest = rest.get(digits..)?.strip_prefix(b" ")?;
    MONTHS
        .iter()
        .find(|month| rest.starts_with(month.as_bytes()))?;
    let rest = rest.get(3..)?.strip_prefix(b" ")?;
    clock(rest).then_some(4 + digits + 1 + 4 + 5)
}

/// [`when`] for a screen reader: `Wednesday 22:30`, `Thursday 12 November 05:00`.
#[must_use]
pub fn spoken_when(t: i64, now: i64, offsets: &Offsets) -> String {
    when_in(t, now, offsets, &WEEKDAYS_LONG, &MONTHS_LONG)
}

fn when_in(
    t: i64,
    now: i64,
    offsets: &Offsets,
    weekdays: &[&str; 7],
    months: &[&str; 12],
) -> String {
    let (day, min) = local(t, offsets.at(t));
    let (today, _) = local(now, offsets.at(now));
    let weekday = weekdays[usize::try_from(day.rem_euclid(7)).unwrap_or(0)];
    match day - today {
        0 => clock_of(min),
        1..=6 => format!("{weekday} {}", clock_of(min)),
        _ => {
            let (_, m, d) = aterm_types::rfc3339::civil_from_days(day);
            format!(
                "{weekday} {d} {} {}",
                months[usize::try_from(m - 1).unwrap_or(0).min(11)],
                clock_of(min)
            )
        }
    }
}

/// The CLI's long date: `Thu Oct 29 09:00`.
#[must_use]
pub fn long_when(t: i64, offsets: &Offsets) -> String {
    let (day, min) = local(t, offsets.at(t));
    let (_, m, d) = aterm_types::rfc3339::civil_from_days(day);
    format!(
        "{} {} {d} {}",
        WEEKDAYS[usize::try_from(day.rem_euclid(7)).unwrap_or(0)],
        MONTHS[usize::try_from(m - 1).unwrap_or(0).min(11)],
        clock_of(min)
    )
}

/// The card's short date: `22 Oct`.
#[must_use]
pub fn day_words(t: i64, offsets: &Offsets) -> String {
    let (day, _) = local(t, offsets.at(t));
    let (_, m, d) = aterm_types::rfc3339::civil_from_days(day);
    format!(
        "{d} {}",
        MONTHS[usize::try_from(m - 1).unwrap_or(0).min(11)]
    )
}

/// The grain of a forecast instant `lead` seconds after the outlook's instant: 10 minutes up
/// to six hours ahead, 30 minutes up to a day, an hour beyond. The `~` and the grain together
/// say how near a forecast is.
#[must_use]
pub const fn grain(lead: i64) -> i64 {
    if lead <= NEAR_LEAD_S {
        ROUND_NEAR_S
    } else if lead <= DAY_LEAD_S {
        ROUND_DAY_S
    } else {
        ROUND_FAR_S
    }
}

/// A forecast start seen at `as_of`, rounded down to its lead's [`grain`] in local time.
#[must_use]
pub fn round_down(t: i64, as_of: i64, offsets: &Offsets) -> i64 {
    t - (t + offsets.at(t)).rem_euclid(grain(t - as_of))
}

/// A forecast end seen at `as_of`, rounded up to its lead's [`grain`] in local time.
#[must_use]
pub fn round_up(t: i64, as_of: i64, offsets: &Offsets) -> i64 {
    let g = grain(t - as_of);
    let r = (t + offsets.at(t)).rem_euclid(g);
    if r == 0 { t } else { t + g - r }
}

/// A span as the card says it: `3d13h`, `10h30m`, `16m`.
#[must_use]
pub fn span_words(s: i64) -> String {
    let s = s.max(0);
    let (d, h, m) = (s / DAY, (s % DAY) / HOUR, (s % HOUR) / 60);
    if d > 0 && h > 0 {
        format!("{d}d{h}h")
    } else if d > 0 {
        format!("{d}d")
    } else if h > 0 && m > 0 {
        format!("{h}h{m:02}m")
    } else if h > 0 {
        format!("{h}h")
    } else {
        format!("{m}m")
    }
}

/// A span for a screen reader: `1 hour 20 minutes`.
#[must_use]
pub fn spoken_span(s: i64) -> String {
    let s = s.max(0);
    let (d, h, m) = (s / DAY, (s % DAY) / HOUR, (s % HOUR) / 60);
    let unit = |n: i64, one: &str| {
        if n == 1 {
            format!("1 {one}")
        } else {
            format!("{n} {one}s")
        }
    };
    let mut parts = Vec::new();
    if d > 0 {
        parts.push(unit(d, "day"));
    }
    if h > 0 {
        parts.push(unit(h, "hour"));
    }
    if m > 0 && d == 0 {
        parts.push(unit(m, "minute"));
    }
    if parts.is_empty() {
        parts.push("0 minutes".into());
    }
    parts.join(" ")
}

fn pct(x10: u32) -> String {
    let x10 = x10.min(1000);
    if x10.is_multiple_of(10) {
        format!("{}%", x10 / 10)
    } else {
        format!("{}.{}%", x10 / 10, x10 % 10)
    }
}

fn whole(x10: u32) -> u32 {
    (x10.min(1000) + 5) / 10
}

// ---------------------------------------------------------------------------------------------
// The bar (§3)

/// One cell of a bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BarCell {
    /// `━` used now.
    Used,
    /// `╍` more use by the window's reset, from the replay.
    More,
    /// `┽` where use stands at the reset (under 100%).
    Tick,
    /// `─` left over at the reset.
    Left,
}

impl BarCell {
    /// Its glyph: procedural box drawing, on the rule's own centre line.
    #[must_use]
    pub const fn glyph(self) -> char {
        match self {
            BarCell::Used => '\u{2501}',
            BarCell::More => '\u{254d}',
            BarCell::Tick => '\u{253d}',
            BarCell::Left => '\u{2500}',
        }
    }

    /// Whether it takes the meter's ink (else the track's).
    #[must_use]
    pub const fn metered(self) -> bool {
        matches!(self, BarCell::Used | BarCell::More | BarCell::Tick)
    }
}

/// `n` cells of a bar of `used_x10` with `projection` at its reset (§3.3): used cells
/// `round(used·n/100)` (at least one while used, at most n−1 while under 100%); no dashes and no
/// tick without a projection; dashes to the end when the projection fills; else a tick at
/// `clamp(round(proj·n/100) − 1, u, n−1)` with dashes up to it and the track after.
#[must_use]
pub fn bar_cells(used_x10: u32, projection: Option<Proj>, n: usize) -> Vec<BarCell> {
    if n == 0 {
        return Vec::new();
    }
    let used = i64::from(used_x10.min(1000));
    let n_i = i64::try_from(n).unwrap_or(i64::MAX);
    let mut u = (used * n_i + 500) / 1000;
    if used > 0 {
        u = u.max(1);
    }
    if used < 1000 {
        u = u.min(n_i - 1);
    }
    let u = usize::try_from(u.clamp(0, n_i)).unwrap_or(0);
    let mut out = vec![BarCell::Left; n];
    for c in out.iter_mut().take(u) {
        *c = BarCell::Used;
    }
    let Some(proj) = projection else {
        return out;
    };
    let at = i64::from(proj.at_reset_x10.min(1000));
    if proj.fills_at.is_some() || at >= 1000 {
        for c in out.iter_mut().skip(u) {
            *c = BarCell::More;
        }
        return out;
    }
    if u >= n {
        return out;
    }
    let tick = ((at * n_i + 500) / 1000 - 1).clamp(i64::try_from(u).unwrap_or(0), n_i - 1);
    let tick = usize::try_from(tick).unwrap_or(u);
    for c in out.iter_mut().take(tick).skip(u) {
        *c = BarCell::More;
    }
    out[tick] = BarCell::Tick;
    out
}

/// The bar as text.
#[must_use]
pub fn bar_text(cells: &[BarCell]) -> String {
    cells.iter().map(|c| c.glyph()).collect()
}

// ---------------------------------------------------------------------------------------------
// The footer's glance (§2)

/// The verdict's words, at each rung's length (§2.2).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct VerdictWords {
    /// The widest form.
    pub full: String,
    /// The short form.
    pub short: String,
    /// The shortest form, where it stays at the narrow rungs.
    pub shortest: Option<String>,
    /// Said in warn ink, its mark leading: `╳` a projected stop, `⧗` a wall that waits now.
    pub warn: bool,
    /// It stays when the rule is narrow (a stop or a wall verdict).
    pub kept_narrow: bool,
}

/// The footer's quantized parts for one pane's row ([`Outlook::glance`]): its bar cells, its
/// reset and 5-hour words to the minute, and the verdict's words. It hashes into the footer's
/// fingerprint, never the raw clock.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Glance {
    /// The week's bar at 10 cells and at 5, where a figure is.
    pub bar: Option<([BarCell; 10], [BarCell; 5])>,
    /// The week's reset, as `♻` words say it (`Thu 09:00`); `None` while a wall stands.
    pub reset: Option<String>,
    /// The 5-hour part (`5h 62% left`).
    pub five: Option<String>,
    /// The verdict, where this pane's tab can be moved and the pace stands.
    pub verdict: Option<VerdictWords>,
    /// The hover title's words on the week (`58% used, ~98% by its reset at today's pace`);
    /// none of a wall's, which the footer's `weekly out` says.
    pub title: Option<String>,
    /// The hover title's verdict clause (`continuous 7d: me → alt1 ~Wed 06:40`; a 5-hour stop's
    /// `stop ~20:10–21:10 (a 5-hour limit)`).
    pub title_verdict: Option<String>,
    /// The spoken sentences after the week's own (`At today's pace about 98% used by then. …`).
    pub spoken: Option<String>,
}

impl Glance {
    /// Keep observed usage and reset information without a projected service promise.
    #[must_use]
    pub fn observations(&self) -> Self {
        let mut view = self.clone();
        if let Some((ten, five)) = view.bar.as_mut() {
            for cell in ten.iter_mut().chain(five.iter_mut()) {
                if *cell != BarCell::Used {
                    *cell = BarCell::Left;
                }
            }
        }
        view.verdict = None;
        view.title = None;
        view.title_verdict = None;
        view.spoken = None;
        view
    }
}

impl Outlook {
    /// A pane-local observation view, retaining the pool's raw snapshot unchanged.
    /// No service chain, proposed target or future-use projection survives the given refusal.
    #[must_use]
    pub fn observations_only(&self, refusal: Refusal) -> Self {
        Self {
            as_of: self.as_of,
            offsets: self.offsets.clone(),
            mode: self.mode,
            credits: self.credits,
            carrier: None,
            demand: self.demand,
            rows: self
                .rows
                .iter()
                .map(|row| {
                    let mut row = row.clone();
                    row.carrier = false;
                    row.walled = None;
                    for window in &mut row.windows {
                        window.projection = None;
                    }
                    row
                })
                .collect(),
            week: None,
            working: None,
            verdict: Verdict::Refused(refusal),
            approx: self.approx.clone(),
            not_needed: Vec::new(),
        }
    }

    fn row(&self, label: &str) -> Option<&RowOutlook> {
        self.rows.iter().find(|r| r.label == label)
    }

    fn when(&self, t: i64) -> String {
        when(t, self.as_of, &self.offsets)
    }

    fn spoken_when(&self, t: i64) -> String {
        spoken_when(t, self.as_of, &self.offsets)
    }

    /// `~T` of a forecast start, rounded down by its lead ([`grain`]).
    fn about(&self, t: i64) -> String {
        format!("~{}", self.when(round_down(t, self.as_of, &self.offsets)))
    }

    /// The spoken `about T` of a forecast start, rounded down by its lead.
    fn spoken_about(&self, t: i64) -> String {
        format!(
            "about {}",
            self.spoken_when(round_down(t, self.as_of, &self.offsets))
        )
    }

    /// A forecast end, rounded up by its lead.
    fn end_at(&self, t: i64) -> i64 {
        round_up(t, self.as_of, &self.offsets)
    }

    /// The end of a stop: the vendor's reset exact, else `~T` rounded up by its lead; `None`
    /// for one nothing ends in reach.
    fn end_words(&self, end: StopEnd) -> Option<String> {
        match end {
            StopEnd::Reset(r) => Some(self.when(r)),
            StopEnd::About(t) => Some(format!("~{}", self.when(self.end_at(t)))),
            StopEnd::Open => None,
        }
    }

    /// [`Self::end_words`] for a screen reader.
    fn spoken_end(&self, end: StopEnd) -> Option<String> {
        match end {
            StopEnd::Reset(r) => Some(self.spoken_when(r)),
            StopEnd::About(t) => Some(self.spoken_when(self.end_at(t))),
            StopEnd::Open => None,
        }
    }

    /// The span a stop is said to last, from its words' own instants; `None` for an open one.
    fn stop_span(&self, from: i64, end: StopEnd) -> Option<i64> {
        let end = match end {
            StopEnd::Reset(r) => r,
            StopEnd::About(t) => self.end_at(t),
            StopEnd::Open => return None,
        };
        Some(end - round_down(from, self.as_of, &self.offsets))
    }

    fn carrier_plan(&self) -> (i64, Option<String>) {
        let row = self.carrier.as_deref().and_then(|c| self.row(c));
        (
            row.map_or(1, |r| i64::from(r.plan.m.max(1))),
            row.and_then(|r| r.plan.title()),
        )
    }

    /// The week pace a day, in whole percent of the carrier's week.
    pub(crate) fn day_pct(&self) -> Option<u32> {
        let pace = self.demand.week.ok()?;
        let (m, _) = self.carrier_plan();
        u32::try_from((pace.x10_m_per_day / m).clamp(0, 1000))
            .ok()
            .map(whole)
    }

    /// The pass's chain up to its first stop (each move with its instant, a row that comes back
    /// with what it had left, a lean with `?`), and what carries the work after that stop.
    fn chain_words(&self, pass: &Pass, lasts: bool) -> (String, Option<String>) {
        let stop = pass.stops.first();
        let mut out = String::new();
        let mut left: BTreeMap<&str, u32> = BTreeMap::new();
        let mut prev: Option<&str> = None;
        for link in pass
            .chain
            .iter()
            .filter(|l| stop.is_none_or(|s| l.to <= s.from))
        {
            match prev {
                None => out.push_str(&link.label),
                Some(p) if p == link.label => {}
                Some(_) => {
                    out.push_str(&format!(
                        " \u{2192} {} {}",
                        link.label,
                        self.about(link.from)
                    ));
                    if let Some(x10) = left.get(link.label.as_str()).filter(|_| lasts) {
                        let last = 100u32.saturating_sub(whole(*x10));
                        out.push_str(&format!(" (its last {last}%)"));
                    }
                }
            }
            if let Some(x10) = link.left_at_x10 {
                left.insert(&link.label, x10);
            }
            prev = Some(&link.label);
        }
        if stop.is_none()
            && let Some(lean) = &pass.lean
        {
            out.push_str(&format!(" \u{2192} {} {} ?", lean.row, self.about(lean.at)));
        }
        let after = stop.and_then(|s| {
            let next = pass.chain.iter().find(|l| l.from >= s.from)?;
            match s.end {
                StopEnd::Reset(r) => Some(format!(
                    "then {} from its reset {}",
                    next.label,
                    self.when(r)
                )),
                StopEnd::About(t) => Some(format!(
                    "then {} from ~{}",
                    next.label,
                    self.when(self.end_at(t))
                )),
                StopEnd::Open => None,
            }
        });
        (out, after)
    }

    /// The verdict's words (§2.2); `None` for a refused pace.
    #[must_use]
    pub fn verdict_words(&self) -> Option<VerdictWords> {
        words_of(self, &self.verdict)
    }

    /// The verdict as spoken sentences.
    #[must_use]
    pub fn verdict_spoken(&self) -> String {
        let cannot = |row: &str| {
            let fresh = self
                .row(row)
                .and_then(|r| r.window(Window::Week))
                .map(|w| w.fresh);
            match fresh {
                Some(Fresh::NeverRead) => "which aterm has never read",
                Some(Fresh::ClockInvalid) => "whose observation time is unconfirmed",
                _ => "which aterm cannot read",
            }
        };
        match &self.verdict {
            Verdict::Walled { to: Some(to), .. } => {
                format!("Walled now: the rotation moves to {to} at the turn's end.")
            }
            Verdict::Walled { to: None, until } => format!(
                "Walled now: the work waits until {}.",
                until.map_or_else(|| "its reset".into(), |u| self.spoken_when(u))
            ),
            Verdict::Stop {
                pass,
                from,
                end,
                lean,
            } => {
                let what = if *pass == PassKind::Working {
                    "A 5-hour stop"
                } else {
                    "A stop"
                };
                let end = self
                    .spoken_end(*end)
                    .map_or_else(|| " with no end in reach".into(), |e| format!(" to {e}"));
                let tail = match lean {
                    Some(row) => format!(", whatever {row}, {}, holds", cannot(row)),
                    None => ", when every account is out".into(),
                };
                format!(
                    "{what} is expected from {}{end}{tail}.",
                    self.spoken_about(*from)
                )
            }
            Verdict::Lean {
                until, row, stop, ..
            } => {
                let worst = stop.map_or_else(String::new, |(from, end)| {
                    let end = self
                        .spoken_end(end)
                        .map_or_else(|| " with no end in reach".into(), |e| format!(" to {e}"));
                    format!(
                        " If {row} has nothing, a stop from {}{end}.",
                        self.spoken_about(from),
                    )
                });
                format!(
                    "Continuous until {}. After that the rotation moves to {row}, {}.{worst}",
                    self.spoken_about(*until),
                    cannot(row)
                )
            }
            Verdict::Credits { at } => format!(
                "Usage credits would carry the work from {}.",
                self.spoken_about(*at)
            ),
            Verdict::Continuous => {
                let moves: Vec<String> = self
                    .week
                    .iter()
                    .flat_map(|p| p.chain.windows(2))
                    .filter(|w| w[0].label != w[1].label)
                    .map(|w| format!("{} around {}", w[1].label, self.spoken_about(w[1].from)))
                    .map(|s| s.replace("around about", "around"))
                    .collect();
                if moves.is_empty() {
                    "Continuous for the next 7 days.".into()
                } else {
                    format!(
                        "Continuous for the next 7 days: the rotation moves to {}.",
                        moves.join(", then ")
                    )
                }
            }
            Verdict::Refused(why) => why.spoken(),
        }
    }

    /// The footer's glance for the pane whose row is `label`, with the footer's own figure of the
    /// week (`used_x10`) where it holds one, so the bar agrees with its words. A tab the rotation
    /// cannot move (`movable` false: another home, not a stored login, not listed) shows no
    /// verdict, and dashes only where its row is the carrier (§2.2).
    #[must_use]
    pub fn glance(&self, label: &str, used_x10: Option<u32>, movable: bool) -> Option<Glance> {
        let row = self.row(label)?;
        let week = row.window(Window::Week)?;
        let clock_admitted = self.as_of > 0
            && week
                .at
                .is_none_or(|at| super::profile::usage_read_age(at, self.as_of).is_some());
        let projection = week
            .projection
            .filter(|_| clock_admitted && self.demand.week.is_ok() && (movable || row.carrier));
        // A wall is `out`, never an inferred 100%: no bar, no percentage.
        let walled = week.fresh == Fresh::Walled;
        let used = clock_admitted
            .then(|| used_x10.or(week.used_x10))
            .flatten()
            .filter(|_| !walled);
        let bar = used.map(|used| {
            let ten = bar_cells(used, projection, 10);
            let five = bar_cells(used, projection, 5);
            (
                std::array::from_fn(|i| ten[i]),
                std::array::from_fn(|i| five[i]),
            )
        });
        let reset = week
            .reset
            .filter(|r| clock_admitted && *r > self.as_of && week.fresh != Fresh::Walled)
            .map(|r| self.when(r));
        // The 5-hour part says a figure of now only: the rule gives it no age, and a 5-hour
        // window moves too fast for an aged one to stand bare.
        let five_now = row
            .window(Window::Five)
            .filter(|w| w.fresh == Fresh::Now)
            .filter(|w| w.reset.is_some_and(|r| r > self.as_of));
        let five = five_now
            .and_then(|w| w.used_x10)
            .map(|x10| format!("5h {} left", pct(1000 - x10.min(1000))));
        let own = row.walled.as_ref().map(|w| Verdict::Walled {
            to: w.to.clone(),
            until: w.until,
        });
        let verdict_of = own.as_ref().unwrap_or(&self.verdict);
        let verdict = movable.then(|| words_of(self, verdict_of)).flatten();
        let title_verdict = verdict.as_ref().map(|w| {
            // A title carries no drawing glyph: a projected stop's `╳` is said as the word. The
            // glass never names the 5-hour window; the title says where its stop comes from.
            let mut full = w
                .full
                .strip_prefix("\u{2573} ")
                .map_or_else(|| w.full.clone(), |rest| format!("stop {rest}"));
            if matches!(
                verdict_of,
                Verdict::Stop {
                    pass: PassKind::Working,
                    ..
                }
            ) {
                full.push_str(" (a 5-hour limit)");
            }
            let chain = self
                .week
                .as_ref()
                .filter(|_| matches!(verdict_of, Verdict::Continuous))
                .map(|p| self.chain_words(p, true).0)
                .filter(|c| c.contains('\u{2192}'));
            match chain {
                Some(chain) => format!("{full}: {chain}"),
                None => full,
            }
        });
        // A wall's week has no title words: the footer's own words say `weekly out`, and the
        // verdict's clause says how long, so the title never repeats `out` or states a figure.
        let mut title = String::new();
        if let Some(used) = used {
            title.push_str(&format!("{} used", pct(used)));
            match projection {
                Some(p) if p.fills_at.is_some() || p.at_reset_x10 >= 1000 => {
                    title.push_str(", out before its reset at today's pace");
                }
                Some(p) => title.push_str(&format!(
                    ", ~{}% by its reset at today's pace",
                    whole(p.at_reset_x10)
                )),
                None => {}
            }
        }
        if let Err(why) = self.demand.week
            && row.carrier
        {
            if !title.is_empty() {
                title.push_str("; ");
            }
            title.push_str(&why.words());
        }
        let mut spoken = String::new();
        let week_reset = week
            .reset
            .filter(|r| clock_admitted && *r > self.as_of && week.fresh != Fresh::Walled);
        if let Some(clause) = self.reset_pace_spoken(week_reset, projection) {
            let mut chars = clause.chars();
            if let Some(first) = chars.next() {
                spoken.extend(first.to_uppercase());
                spoken.push_str(chars.as_str());
                spoken.push_str(". ");
            }
        }
        if let Some(five) = five_now
            && let (Some(x10), Some(reset)) = (five.used_x10, five.reset)
        {
            spoken.push_str(&format!(
                "5-hour {}% left, resets {}. ",
                whole(1000 - x10.min(1000)),
                self.spoken_when(reset)
            ));
        }
        if movable {
            match own {
                Some(v) => spoken.push_str(
                    &Outlook {
                        verdict: v,
                        ..self.clone()
                    }
                    .verdict_spoken(),
                ),
                None => spoken.push_str(&self.verdict_spoken()),
            }
        }
        let spoken = spoken.trim_end().to_owned();
        Some(Glance {
            bar,
            reset,
            five,
            verdict,
            title: (!title.is_empty()).then_some(title),
            title_verdict,
            spoken: (!spoken.is_empty()).then_some(spoken),
        })
    }
}

fn words_of(out: &Outlook, verdict: &Verdict) -> Option<VerdictWords> {
    let words = |full: String, short: String, shortest: Option<String>, warn: bool| {
        Some(VerdictWords {
            kept_narrow: shortest.is_some(),
            full,
            short,
            shortest,
            warn,
        })
    };
    match verdict {
        Verdict::Walled { to: Some(to), .. } => words(
            format!("\u{2192} {to} at turn end"),
            format!("\u{2192} {to}"),
            Some(format!("\u{2192} {to}")),
            false,
        ),
        Verdict::Walled { to: None, until } => {
            let until = until.map_or_else(|| "its reset".into(), |u| out.when(u));
            words(
                format!("\u{29d7} waits to {until}"),
                format!("\u{29d7} to {until}"),
                Some(format!("\u{29d7} to {until}")),
                true,
            )
        }
        // A projected stop is `╳` whichever pass found it (`⧗` is a wall standing now), and
        // the 5-hour window is never named here (§2.3).
        Verdict::Stop {
            from, end, lean, ..
        } => {
            let start = out.about(*from);
            let q = if lean.is_some() { " ?" } else { "" };
            let short = format!("\u{2573} {start}{q}");
            let full = out.end_words(*end).map_or_else(
                || format!("\u{2573} from {start}{q}"),
                |end| format!("\u{2573} {start}\u{2013}{end}{q}"),
            );
            let short = if end.at().is_none() {
                full.clone()
            } else {
                short
            };
            words(full, short.clone(), Some(short), true)
        }
        Verdict::Lean { until, row, .. } => words(
            format!("continuous to {}, then {row} ?", out.about(*until)),
            format!("to {} ?", out.about(*until)),
            None,
            false,
        ),
        Verdict::Credits { at } => words(
            format!("\u{2192} credits {}", out.about(*at)),
            "\u{2192} credits".into(),
            None,
            false,
        ),
        Verdict::Continuous => words("continuous 7d".into(), "continuous 7d".into(), None, false),
        Verdict::Refused(_) => None,
    }
}

// ---------------------------------------------------------------------------------------------
// The card (§8)

/// A card cell's ink ([`CardRow::Plot`]): the GUI floors each against the card's ground.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CardInk {
    /// The meter: `━ ╍ ┽` and a carrier's run.
    Meter,
    /// The track: `─ ┄`.
    Track,
    /// A stop: `╳`.
    Warn,
    /// A label's text and a figure.
    Value,
    /// The axis and the legend's words.
    Axis,
}

/// One row of the Accounts card, in the outlook's words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CardRow {
    /// A line of words.
    Note {
        /// The words.
        text: String,
        /// What a reader hears, where it differs.
        spoken: Option<String>,
        /// Said in warn ink, its `╳` (a projected stop) or `⧗` (a wall now) leading.
        warn: bool,
        /// Said in value ink (else the label's).
        value: bool,
    },
    /// A drawn row with its spoken caption.
    Plot {
        /// Its cells.
        cells: Vec<(char, CardInk)>,
        /// Its caption.
        spoken: String,
    },
    /// One roster row's item: the GUI keeps its act, its check and its place.
    Account {
        /// The roster label.
        label: String,
        /// The item's words.
        text: String,
        /// What a reader hears.
        spoken: String,
    },
    /// A rule.
    Rule,
}

impl CardRow {
    fn note(text: impl Into<String>) -> CardRow {
        CardRow::Note {
            text: text.into(),
            spoken: None,
            warn: false,
            value: false,
        }
    }

    /// The cells it takes ([`CARD_ITEM_LEAD`] for an item's glyph).
    #[must_use]
    pub fn width(&self) -> usize {
        match self {
            CardRow::Note { text, .. } => super::footer::text_width(text),
            CardRow::Account { text, .. } => super::footer::text_width(text) + CARD_ITEM_LEAD,
            CardRow::Plot { cells, .. } => cells.len(),
            CardRow::Rule => 0,
        }
    }
}

/// One roster row on the card while [`Outlook::card_rows`] lays it out: its item, its item with
/// its bars folded in, its bars by window, and whether it carries the work.
type CardItem = (CardRow, CardRow, Vec<(Window, CardRow)>, bool);

/// The cells an item's glyph and gap take before its words (`⎆  `).
pub const CARD_ITEM_LEAD: usize = 3;

/// How a bar row sits in the card: its window's word, its cells, its figure, its reset.
fn bar_row(
    window: Window,
    cells: &[BarCell],
    figure: &str,
    reset: &str,
    spoken: String,
) -> CardRow {
    let mut out: Vec<(char, CardInk)> = Vec::new();
    let mut text = |t: &str, ink: CardInk| out.extend(t.chars().map(|c| (c, ink)));
    text("   ", CardInk::Axis);
    text(&format!("{:<5}", window.word()), CardInk::Axis);
    for c in cells {
        let ink = if c.metered() {
            CardInk::Meter
        } else {
            CardInk::Track
        };
        text(&c.glyph().to_string(), ink);
    }
    text(&format!("{figure:>6}"), CardInk::Value);
    if !reset.is_empty() {
        text("  ", CardInk::Axis);
        text(reset, CardInk::Axis);
    }
    CardRow::Plot { cells: out, spoken }
}

impl Outlook {
    fn age(&self, at: Option<i64>) -> String {
        span_words(self.as_of - at.unwrap_or(self.as_of))
    }

    fn class_word(&self, row: &RowOutlook, short: bool) -> String {
        let Some(week) = row.window(Window::Week) else {
            return String::new();
        };
        let since =
            |t: Option<i64>| t.map_or_else(String::new, |t| format!(" since {}", self.when(t)));
        match week.fresh {
            Fresh::Now => "now".into(),
            Fresh::AsOf => format!("as of {}", self.age(week.at)),
            Fresh::Stale => format!("read {} ago while in use", self.age(week.at)),
            Fresh::IdleHere if short => format!("idle here {}", self.age(week.at)),
            Fresh::IdleHere => format!("idle here{}", since(week.at)),
            Fresh::UsedSince if short => format!("used since {}", self.age(week.at)),
            Fresh::UsedSince => format!("used here{}", since(week.at)),
            Fresh::Pool if short => format!("as of {}", self.age(week.at)),
            Fresh::Pool => format!("as of {}, others may use it", self.age(week.at)),
            Fresh::ResetUnread if short => "not read since reset".into(),
            Fresh::ResetUnread => "reset passed, not read since".into(),
            Fresh::NeverRead => "never read".into(),
            Fresh::ClockInvalid => "observation time unconfirmed".into(),
            Fresh::Walled => format!(
                "limited to {}",
                row.walled
                    .as_ref()
                    .and_then(|w| w.until)
                    .or(match row.condition {
                        Condition::Limited { until } => Some(until),
                        _ => None,
                    })
                    .map_or_else(|| "its reset".into(), |u| self.when(u))
            ),
            Fresh::SignedOut => "signed out".into(),
            Fresh::NotEnrolled => "not enrolled".into(),
        }
    }

    /// What a reader hears of a row's class: its age, then its class.
    fn class_spoken(&self, row: &RowOutlook) -> String {
        let Some(week) = row.window(Window::Week) else {
            return String::new();
        };
        let ago = |at: Option<i64>| {
            format!(
                "read {} ago",
                spoken_span(self.as_of - at.unwrap_or(self.as_of))
            )
        };
        match week.fresh {
            Fresh::Now => "read now".into(),
            Fresh::AsOf | Fresh::Stale => format!("{}, while in use", ago(week.at)),
            Fresh::IdleHere => format!("{}, idle here since", ago(week.at)),
            Fresh::UsedSince => format!("{}, used here since", ago(week.at)),
            Fresh::Pool => format!("{}, others may use it", ago(week.at)),
            _ => self.class_word(row, false),
        }
    }

    fn figure_words(week: &WindowOutlook) -> String {
        match (week.fresh, week.used_x10) {
            (Fresh::Walled, _) => "0%".into(),
            (f, Some(x10)) if f.bounded() => {
                format!("\u{2264}{}", pct(1000 - x10.min(1000)))
            }
            (f, Some(x10)) if f.shows_figure() => pct(1000 - x10.min(1000)),
            _ => "\u{2014}".into(),
        }
    }

    fn reset_words(&self, w: &WindowOutlook) -> String {
        if !w
            .at
            .is_some_and(|at| super::profile::usage_read_age(at, self.as_of).is_some())
        {
            return String::new();
        }
        match (w.fresh, w.reset) {
            (Fresh::ResetUnread, Some(r)) => {
                format!("reset {}", day_words(r, &self.offsets))
            }
            (_, Some(r)) if r > self.as_of => format!("\u{267b} {}", self.when(r)),
            _ => String::new(),
        }
    }

    fn bar_spoken(&self, row: &RowOutlook, w: &WindowOutlook, proj: Option<Proj>) -> String {
        let name = if w.window == Window::Week {
            "week"
        } else {
            "5-hour"
        };
        match (w.fresh, w.used_x10) {
            (_, Some(x10)) => {
                let mut out = format!(
                    "{}, {name}: {} percent used, {} left",
                    row.label,
                    whole(x10),
                    whole(1000 - x10.min(1000))
                );
                let reset = w.reset.filter(|r| *r > self.as_of);
                if let Some(clause) = self.reset_pace_spoken(reset, proj) {
                    out.push_str(", ");
                    out.push_str(&clause);
                }
                out.push('.');
                out
            }
            _ => format!("{}, {name}: no figure.", row.label),
        }
    }

    /// What a reader hears of a window's reset and where today's pace takes it by then
    /// (`resets Thursday 09:00; at today's pace about 98 percent used by then`): the one source
    /// of the footer's spoken week and the card's bar captions.
    fn reset_pace_spoken(&self, reset: Option<i64>, proj: Option<Proj>) -> Option<String> {
        let reset = reset.map(|r| format!("resets {}", self.spoken_when(r)));
        let pace = proj.map(|p| {
            if p.fills_at.is_some() || p.at_reset_x10 >= 1000 {
                "at today's pace it runs out before then".to_string()
            } else {
                format!(
                    "at today's pace about {} percent used by then",
                    whole(p.at_reset_x10)
                )
            }
        });
        match (reset, pace) {
            (Some(reset), Some(pace)) => Some(format!("{reset}; {pace}")),
            (reset, pace) => reset.or(pace),
        }
    }

    /// The 7-day strip at `cells` cells after `lead` cells of margin, and its axis (§8.3).
    fn strip(&self, cells: usize, lead: usize) -> Option<(CardRow, CardRow)> {
        let pass = self.week.as_ref()?;
        if cells == 0 {
            return None;
        }
        let start = self.as_of;
        let span = accounts::WEEK_WINDOW_S;
        let per = span / i64::try_from(cells).unwrap_or(1);
        let cell_at = |c: usize| start + per * i64::try_from(c).unwrap_or(0);
        let covers = |from: i64, to: i64, c: usize| {
            let (a, b) = (cell_at(c), cell_at(c) + per);
            (to.min(b) - from.max(a)) * 2 >= per
        };
        #[derive(Clone, PartialEq)]
        enum Holds {
            Row(String),
            Stop,
            Lean,
            Empty,
        }
        let holder: Vec<Holds> = (0..cells)
            .map(|c| {
                if pass
                    .lean
                    .as_ref()
                    .is_some_and(|l| cell_at(c) + per / 2 >= l.at)
                {
                    Holds::Lean
                } else if pass
                    .stops
                    .iter()
                    .any(|s| covers(s.from, s.end.at().unwrap_or(i64::MAX), c))
                {
                    Holds::Stop
                } else if let Some(link) = pass.chain.iter().find(|l| covers(l.from, l.to, c)) {
                    Holds::Row(link.label.clone())
                } else {
                    Holds::Empty
                }
            })
            .collect();
        let mut out: Vec<(char, CardInk)> = vec![(' ', CardInk::Axis); lead];
        let mut c = 0;
        while c < cells {
            match &holder[c] {
                Holds::Stop => {
                    out.push(('\u{2573}', CardInk::Warn));
                    c += 1;
                }
                Holds::Row(label) => {
                    let run = holder[c..].iter().take_while(|h| *h == &holder[c]).count();
                    let text: Vec<char> = label.chars().collect();
                    if text.len() <= run {
                        out.extend(text.iter().map(|ch| (*ch, CardInk::Value)));
                        out.extend((text.len()..run).map(|_| ('\u{2501}', CardInk::Meter)));
                    } else {
                        out.push(('\u{2502}', CardInk::Meter));
                        out.extend((1..run).map(|_| ('\u{2501}', CardInk::Meter)));
                    }
                    c += run;
                }
                Holds::Lean => {
                    let rest = cells - c;
                    let mut text: Vec<char> = pass
                        .lean
                        .as_ref()
                        .map(|l| l.row.chars().collect())
                        .unwrap_or_default();
                    text.push('?');
                    text.truncate(rest);
                    out.extend(text.iter().map(|ch| (*ch, CardInk::Value)));
                    out.extend((text.len()..rest).map(|_| ('\u{2504}', CardInk::Track)));
                    c = cells;
                }
                Holds::Empty => {
                    out.push(('\u{2500}', CardInk::Track));
                    c += 1;
                }
            }
        }
        // The axis: `now`, then each day named at its noon where it fits.
        let mut axis: Vec<(char, CardInk)> = vec![(' ', CardInk::Axis); lead + cells];
        for (i, ch) in "now".chars().enumerate() {
            axis[lead + i] = (ch, CardInk::Axis);
        }
        let (today, _) = local(start, self.offsets.at(start));
        let mut named: Vec<&str> = Vec::new();
        for d in 1..=7 {
            // Each day's noon on its own clock (a daylight-saving change moves it an hour).
            let guess = (today + d) * DAY + 12 * HOUR - self.offsets.at(start);
            let noon = (today + d) * DAY + 12 * HOUR - self.offsets.at(guess);
            if noon >= start + span {
                break;
            }
            let at = usize::try_from((noon - start) / per).unwrap_or(usize::MAX);
            let begin = (lead + at).saturating_sub(1);
            if begin < lead + 4 || begin + 3 > lead + cells || axis[begin - 1].0 != ' ' {
                continue;
            }
            let day = usize::try_from((today + d).rem_euclid(7)).unwrap_or(0);
            for (i, ch) in WEEKDAYS[day].chars().enumerate() {
                axis[begin + i] = (ch, CardInk::Axis);
            }
            named.push(WEEKDAYS_LONG[day]);
        }
        Some((
            CardRow::Plot {
                cells: out,
                spoken: self.strip_spoken(pass),
            },
            CardRow::Plot {
                cells: axis,
                spoken: format!("Days: now, then {} at noon.", named.join(", ")),
            },
        ))
    }

    /// The strip's caption: the chain in order, with its stops and its lean.
    fn strip_spoken(&self, pass: &Pass) -> String {
        enum Part<'a> {
            Link(&'a Link),
            Stop(&'a Stop),
        }
        let mut parts: Vec<(i64, Part<'_>)> = pass
            .chain
            .iter()
            .filter(|l| l.to > l.from)
            .map(|l| (l.from, Part::Link(l)))
            .chain(pass.stops.iter().map(|s| (s.from, Part::Stop(s))))
            .collect();
        parts.sort_by_key(|(t, _)| *t);
        let mut said: Vec<String> = Vec::new();
        let mut last: Option<&str> = None;
        let lean_at = pass.lean.as_ref().map(|l| l.at);
        for (i, (_, part)) in parts.iter().enumerate() {
            let next_from = parts.get(i + 1).map(|(t, _)| *t).or(lean_at);
            match part {
                Part::Link(link) => {
                    if last == Some(link.label.as_str()) {
                        if let (Some(t), Some(prev)) = (next_from, said.last_mut()) {
                            *prev = format!("{} until {}", link.label, self.spoken_about(t));
                        }
                        continue;
                    }
                    last = Some(&link.label);
                    said.push(match next_from {
                        Some(t) => format!("{} until {}", link.label, self.spoken_about(t)),
                        None => link.label.clone(),
                    });
                }
                Part::Stop(stop) => {
                    last = None;
                    said.push(self.spoken_end(stop.end).map_or_else(
                        || "a stop with no end in reach".into(),
                        |end| format!("a stop until {end}"),
                    ));
                }
            }
        }
        let mut out = format!("Next 7 days at today's pace: {}", said.join(", then "));
        if let Some(lean) = &pass.lean {
            let cause = match lean.why {
                Fresh::NeverRead => "which aterm has never read",
                Fresh::ClockInvalid => "whose observation time is unconfirmed",
                _ => "which aterm cannot read",
            };
            out.push_str(&format!(", then {}, {cause}", lean.row));
        } else if pass.stops.is_empty() {
            out.push_str("; no stop");
        }
        out.push('.');
        out
    }

    fn verdict_row(&self, room: usize, narrow: bool) -> CardRow {
        let spoken = Some(self.verdict_spoken());
        let note = |cands: Vec<String>, warn: bool| CardRow::Note {
            text: first_fit(cands, room),
            spoken: spoken.clone(),
            warn,
            value: !warn,
        };
        if narrow {
            return match (&self.verdict, self.verdict_words()) {
                (Verdict::Refused(why), _) => {
                    note(vec![format!("\u{21bb}  {}", why.words())], false)
                }
                (_, Some(w)) if w.warn => note(vec![w.full, w.short], true),
                (_, Some(w)) => note(
                    vec![
                        format!("\u{21bb}  {}", w.full),
                        format!("\u{21bb}  {}", w.short),
                    ],
                    false,
                ),
                (_, None) => note(vec![String::new()], false),
            };
        }
        match &self.verdict {
            Verdict::Stop {
                pass, from, end, ..
            } => {
                let what = if *pass == PassKind::Working {
                    "5h stop"
                } else {
                    "Stop"
                };
                let span = self.stop_span(*from, *end);
                let from = self.about(*from);
                let forms = match (self.end_words(*end), span) {
                    (Some(to), Some(span)) => {
                        let stop = format!("\u{2573}  {what} {from} \u{2013} {to}");
                        let span = span_words(span);
                        vec![
                            format!("{stop} ({span}): every account out"),
                            format!("{stop} ({span})"),
                            stop,
                        ]
                    }
                    _ => {
                        let stop = format!("\u{2573}  {what} from {from}");
                        vec![format!("{stop}: every account out"), stop]
                    }
                };
                note(forms, true)
            }
            Verdict::Walled { to: None, .. } => note(
                vec![format!(
                    "\u{29d7}  Walled now: {}",
                    self.verdict_words()
                        .map(|w| w.full.trim_start_matches("\u{29d7} ").to_owned())
                        .unwrap_or_default()
                )],
                true,
            ),
            Verdict::Walled { to: Some(to), .. } => note(
                vec![
                    format!("\u{21bb}  Walled now: \u{2192} {to} at turn end"),
                    format!("\u{21bb}  Walled: \u{2192} {to}"),
                ],
                false,
            ),
            Verdict::Lean {
                until, row, why, ..
            } => {
                let head = format!("\u{21bb}  Continuous to {}, then {row}", self.about(*until));
                note(
                    vec![
                        format!(
                            "{head} ({})",
                            match why {
                                Fresh::NeverRead => "never read",
                                Fresh::ClockInvalid => "observation time unconfirmed",
                                f if f.bounded() => "not read lately",
                                _ => "not read",
                            }
                        ),
                        format!("{head} ?"),
                    ],
                    false,
                )
            }
            Verdict::Refused(why) => note(vec![format!("\u{21bb}  {}", why.words())], false),
            Verdict::Credits { at } => note(
                vec![format!("\u{21bb}  Usage credits from {}", self.about(*at))],
                false,
            ),
            Verdict::Continuous => note(
                vec![
                    "\u{21bb}  Continuous for the next 7 days".into(),
                    "\u{21bb}  Continuous 7 days".into(),
                ],
                false,
            ),
        }
    }

    /// The Accounts card's rows for the pane whose row is `label` (§8), each at most `room` cells
    /// wide and, where `height` is given, at most that many rows in all: the verdict and the
    /// chain, the 7-day strip, the legend, then one [`CardRow::Account`] per roster row in roster
    /// order with its bars. Rows give way to height in §8.4's order; the verdict lines and the
    /// items stay.
    #[must_use]
    pub fn card_rows(
        &self,
        label: Option<&str>,
        room: usize,
        height: Option<usize>,
    ) -> Vec<CardRow> {
        let wide = room >= 60;
        let narrow = room <= 30;
        let bar_n = if wide { 30 } else { 16 };
        let strip_n = if wide { 56 } else { 42 };
        let note = |cands: Vec<String>| CardRow::note(first_fit(cands, room));
        // A projected stop's `╳` is drawn, never heard: a reader hears `a stop`.
        let stop_note = |cands: Vec<String>| {
            let text = first_fit(cands, room);
            CardRow::Note {
                spoken: Some(text.trim_start().replace('\u{2573}', "a stop")),
                text,
                warn: false,
                value: false,
            }
        };
        let mut head: Vec<CardRow> = vec![self.verdict_row(room, narrow)];
        // Notes that give way to height first (§15: the design's mockups leave them out).
        let mut extra: Vec<CardRow> = Vec::new();
        if !narrow {
            if let Verdict::Lean { stop, row, why, .. } = &self.verdict {
                if let Some((from, end)) = stop {
                    let from = self.about(*from);
                    head.push(stop_note(match self.end_words(*end) {
                        Some(to) => vec![
                            format!("   if {row} has nothing: \u{2573} {from} \u{2013} {to}"),
                            format!("   if nothing: \u{2573} {from} \u{2013} {to}"),
                            format!("   if nothing: \u{2573} {from}"),
                        ],
                        None => vec![
                            format!("   if {row} has nothing: \u{2573} from {from}"),
                            format!("   if nothing: \u{2573} from {from}"),
                        ],
                    }));
                }
                head.push(note(match why {
                    f if f.bounded() => vec![
                        format!("   if {row} holds what was read: continuous for 7 days"),
                        "   if it holds what was read: continuous 7d".into(),
                    ],
                    _ => vec![
                        format!("   if {row} has a fresh week: continuous for 7 days"),
                        "   if a fresh week: continuous 7d".into(),
                    ],
                }));
            }
            if let Some(pass) = &self.week {
                let (chain, after) = self.chain_words(pass, true);
                let (bare, _) = self.chain_words(pass, false);
                let clause = match (wide, self.day_pct(), after.is_some()) {
                    (true, Some(day), false) => format!(" \u{b7} at today's pace ({day}% a day)"),
                    (true, _, _) => " \u{b7} at today's pace".to_string(),
                    (false, _, _) => " \u{b7} today's pace".to_string(),
                };
                match after {
                    None => head.push(note(vec![
                        format!("   {chain}{clause}"),
                        format!("   {chain}"),
                        format!("   {bare}"),
                    ])),
                    Some(after) => {
                        head.push(note(vec![format!("   {chain}"), format!("   {bare}")]));
                        head.push(note(vec![
                            format!("   {after}{clause}"),
                            format!("   {after}"),
                        ]));
                    }
                }
            }
            let five_stop = matches!(
                self.verdict,
                Verdict::Stop {
                    pass: PassKind::Working,
                    ..
                }
            );
            let five = match (&self.working, self.demand.five) {
                (_, Err(Refusal::Idle)) => Some(note(vec!["   5h: idle".to_string()])),
                (Some(w), _) if w.stops.is_empty() => Some(note(vec![
                    "   5h: no stop in the next 12h at this hour's pace".into(),
                    "   5h: no stop in the next 12h".into(),
                ])),
                // The verdict says the stop; the 5-hour line says what carries the work after it.
                (Some(w), _) if five_stop => self
                    .chain_words(w, false)
                    .1
                    .map(|after| note(vec![format!("   5h: {after}")])),
                (Some(w), _) => w.stops.first().map(|s| {
                    let from = self.about(s.from);
                    stop_note(vec![self.end_words(s.end).map_or_else(
                        || format!("   5h: \u{2573} from {from}"),
                        |to| format!("   5h: \u{2573} {from} \u{2013} {to}"),
                    )])
                }),
                (None, Err(why)) => Some(note(vec![format!("   5h: {}", why.words())])),
                (None, Ok(_)) => None,
            };
            head.extend(five);
            if wide {
                if !self.not_needed.is_empty() {
                    extra.push(CardRow::note(format!(
                        "   not needed: {}",
                        self.not_needed.join(", ")
                    )));
                }
                for a in &self.approx {
                    extra.push(note(
                        self.approx_words(a, true)
                            .into_iter()
                            .map(|w| format!("   \u{2248} {w}"))
                            .collect(),
                    ));
                }
            }
        }
        let lead = 4usize.min(room.saturating_sub(strip_n));
        let strip = (!narrow).then(|| self.strip(strip_n, lead)).flatten();
        // The legend at 60 cells only: the 44-cell card of §8.2 has none.
        let legend = wide.then(|| {
            let mut c: Vec<(char, CardInk)> = Vec::new();
            let mut text = |t: &str, ink: CardInk| c.extend(t.chars().map(|ch| (ch, ink)));
            text("   ", CardInk::Axis);
            text("\u{2501}", CardInk::Meter);
            text(" used now  ", CardInk::Axis);
            text("\u{254d}", CardInk::Meter);
            text(" more by its reset at today's pace  ", CardInk::Axis);
            text("\u{2500}", CardInk::Track);
            text(" left", CardInk::Axis);
            CardRow::Plot {
                cells: c,
                spoken: "Legend: heavy, used now; dashed, more by its reset at today's pace; \
                         light, left."
                    .into(),
            }
        });
        // The accounts, in roster order: (item, its folded item, its bars, carrier).
        let mut items: Vec<CardItem> = Vec::new();
        for row in &self.rows {
            let Some(week) = row.window(Window::Week) else {
                continue;
            };
            let plan = row.plan.title().unwrap_or_else(|| "plan ?".into());
            let figure = week.fresh.shows_figure().then(|| Self::figure_words(week));
            let reset = self.reset_words(week);
            let short_item = |with_reset: bool| match &figure {
                Some(f) if with_reset && !reset.is_empty() => {
                    format!("{} \u{b7} {f} left \u{b7} {reset}", row.label)
                }
                Some(f) => format!("{} \u{b7} {f} left", row.label),
                None => format!("{} \u{b7} {}", row.label, self.class_word(row, true)),
            };
            let text = if narrow {
                let full = short_item(true);
                if super::footer::text_width(&full) + CARD_ITEM_LEAD <= room {
                    full
                } else {
                    short_item(false)
                }
            } else {
                let mut t = match (row.class, wide) {
                    (Class::Pool, true) => format!(
                        "{} \u{b7} pool \u{b7} {plan} \u{b7} {}",
                        row.label,
                        self.class_word(row, false)
                    ),
                    (Class::Pool, false) => {
                        format!(
                            "{} \u{b7} pool \u{b7} {}",
                            row.label,
                            self.class_word(row, true)
                        )
                    }
                    (Class::Preferred, true) => format!(
                        "{} \u{b7} {plan} \u{b7} preferred \u{b7} {}",
                        row.label,
                        self.class_word(row, false)
                    ),
                    (Class::Preferred, false) => {
                        format!(
                            "{} \u{b7} {plan} \u{b7} {}",
                            row.label,
                            self.class_word(row, true)
                        )
                    }
                };
                if wide && label == Some(row.label.as_str()) {
                    t.push_str(" \u{b7} this tab");
                }
                t
            };
            let spoken = format!(
                "{}, {}, {}, {}, {}.",
                row.label,
                row.plan.title().unwrap_or_else(|| "plan not seen".into()),
                match row.class {
                    Class::Preferred => "preferred",
                    Class::Pool => "pool",
                },
                match (week.fresh.shows_figure(), week.used_x10) {
                    (true, Some(x10)) if week.fresh == Fresh::Walled => {
                        let _ = x10;
                        "0 percent left".to_string()
                    }
                    (true, Some(x10)) if week.fresh.bounded() => {
                        format!("at most {} percent left", whole(1000 - x10.min(1000)))
                    }
                    (true, Some(x10)) => format!("{} percent left", whole(1000 - x10.min(1000))),
                    _ => "no figure".into(),
                },
                self.class_spoken(row)
            );
            let item = CardRow::Account {
                label: row.label.clone(),
                text,
                spoken: spoken.clone(),
            };
            let folded = CardRow::Account {
                label: row.label.clone(),
                text: short_item(true),
                spoken,
            };
            let mut bars = Vec::new();
            if !narrow {
                if let Some(used) = week.used_x10.filter(|_| week.fresh.shows_figure()) {
                    let proj = week.projection.filter(|_| self.demand.week.is_ok());
                    let used = if week.fresh == Fresh::Walled {
                        1000
                    } else {
                        used
                    };
                    bars.push((
                        Window::Week,
                        bar_row(
                            Window::Week,
                            &bar_cells(used, proj, bar_n),
                            &Self::figure_words(week),
                            &reset,
                            self.bar_spoken(row, week, proj),
                        ),
                    ));
                }
                if let Some(five) = row.window(Window::Five)
                    && let Some(used) = five.used_x10.filter(|_| five.fresh.shows_figure())
                    && five.reset.is_some_and(|r| r > self.as_of)
                    && (row.carrier
                        || five.fresh == Fresh::Walled
                        || whole(used) >= accounts::LOW_SESSION_PCT)
                {
                    let proj = five.projection.filter(|_| self.demand.five.is_ok());
                    bars.push((
                        Window::Five,
                        bar_row(
                            Window::Five,
                            &bar_cells(used, proj, bar_n),
                            &Self::figure_words(five),
                            &self.reset_words(five),
                            self.bar_spoken(row, five, proj),
                        ),
                    ));
                }
            }
            items.push((item, folded, bars, row.carrier));
        }
        // The height ladder (§8.4), after the extra notes: the strip with its rule, the legend
        // with its rule, the other rows' bars (folded into their items), the carrier's 5-hour
        // bar, the carrier's week bar.
        let kept = |w: Window, carrier: bool, keep: &[bool; 6]| match (carrier, w) {
            (false, _) => keep[3],
            (true, Window::Five) => keep[4],
            (true, Window::Week) => keep[5],
        };
        let mut keep = [true; 6];
        keep[1] = strip.is_some();
        keep[2] = legend.is_some();
        let count = |keep: &[bool; 6]| {
            let mut n = head.len() + if keep[0] { extra.len() } else { 0 };
            n += if keep[1] { 3 } else { 0 } + usize::from(keep[2]) + usize::from(!narrow);
            for (_, _, bars, carrier) in &items {
                n += 1;
                n += bars
                    .iter()
                    .filter(|(w, _)| kept(*w, *carrier, keep))
                    .count();
            }
            n
        };
        if let Some(h) = height {
            for step in 0..6 {
                if count(&keep) <= h {
                    break;
                }
                keep[step] = false;
            }
        }
        let mut out = head;
        if keep[0] {
            out.extend(extra);
        }
        if let (true, Some((strip, axis))) = (keep[1], strip) {
            out.push(CardRow::Rule);
            out.push(strip);
            out.push(axis);
        }
        // The rule above the items stays with them; the legend sits under it.
        if !narrow {
            out.push(CardRow::Rule);
        }
        if let (true, Some(legend)) = (keep[2], legend) {
            out.push(legend);
        }
        for (item, folded, bars, carrier) in items {
            let fold = !carrier && !keep[3] && !bars.is_empty();
            out.push(if fold { folded } else { item });
            out.extend(
                bars.into_iter()
                    .filter(|(w, _)| kept(*w, carrier, &keep))
                    .map(|(_, bar)| bar),
            );
        }
        out.into_iter().map(|row| fit_card_row(row, room)).collect()
    }
}

/// The first of `cands` that fits in `room` cells, else the last (which [`fit_card_row`] cuts).
fn first_fit(cands: Vec<String>, room: usize) -> String {
    let last = cands.len().saturating_sub(1);
    cands
        .into_iter()
        .enumerate()
        .find(|(i, c)| *i == last || super::footer::text_width(c) <= room)
        .map(|(_, c)| c)
        .unwrap_or_default()
}

fn fit_card_row(row: CardRow, room: usize) -> CardRow {
    let cut = |text: String, room: usize| -> String {
        if super::footer::text_width(&text) <= room {
            return text;
        }
        let mut out: String = aterm_grapheme::grid_graphemes(&text)
            .take_while(|c| c.end_col < room)
            .map(|c| c.text)
            .collect();
        out.push('\u{2026}');
        out
    };
    match row {
        CardRow::Note {
            text,
            spoken,
            warn,
            value,
        } => {
            let said = spoken.or_else(|| {
                (super::footer::text_width(&text) > room).then(|| text.trim().to_owned())
            });
            CardRow::Note {
                text: cut(text, room),
                spoken: said,
                warn,
                value,
            }
        }
        CardRow::Account {
            label,
            text,
            spoken,
        } => CardRow::Account {
            label,
            text: cut(text, room.saturating_sub(CARD_ITEM_LEAD)),
            spoken,
        },
        CardRow::Plot { mut cells, spoken } => {
            cells.truncate(room);
            CardRow::Plot { cells, spoken }
        }
        CardRow::Rule => CardRow::Rule,
    }
}

// ---------------------------------------------------------------------------------------------
// The CLI (§9)

impl Outlook {
    /// An instant as the CLI's window lines say it: the card's words within a day, the long date
    /// beyond.
    fn cli_when(&self, t: i64) -> String {
        if (t - self.as_of).abs() < DAY {
            self.when(t)
        } else {
            long_when(t, &self.offsets)
        }
    }

    fn window_line(&self, row: &RowOutlook, w: &WindowOutlook) -> String {
        let mut out = format!("       {:<5} ", w.window.word());
        if w.fresh == Fresh::ClockInvalid {
            out.push_str("\u{2014} \u{b7} observation time unconfirmed");
            return out;
        }
        let closed =
            w.at.is_some_and(|at| super::profile::usage_read_age(at, self.as_of).is_some())
                && w.reset.is_some_and(|r| r <= self.as_of);
        if w.window == Window::Five && closed {
            out.push_str(&format!(
                "reset {} passed: opens on first use",
                w.reset.map_or_else(String::new, |r| self.cli_when(r))
            ));
            return out;
        }
        match w.fresh {
            Fresh::ResetUnread => {
                out.push_str(&format!(
                    "\u{2014} \u{b7} reset {} passed, not read since",
                    w.reset
                        .map_or_else(String::new, |r| long_when(r, &self.offsets))
                ));
                return out;
            }
            Fresh::NeverRead | Fresh::SignedOut | Fresh::NotEnrolled => {
                out.push_str(&format!(
                    "\u{2014} \u{b7} {}",
                    w.fresh.word().replace('-', " ")
                ));
                return out;
            }
            _ => {}
        }
        let Some(used) = w.used_x10 else {
            out.push('\u{2014}');
            return out;
        };
        let lower = w.fresh.bounded();
        let (ge, le) = if lower {
            ("\u{2265}", "\u{2264}")
        } else {
            ("", "")
        };
        out.push_str(&format!(
            "{ge}{} used \u{b7} {le}{} left",
            pct(used),
            pct(1000 - used.min(1000))
        ));
        if let Some(r) = w.reset.filter(|r| *r > self.as_of) {
            out.push_str(&format!(
                " \u{b7} \u{267b} {} (in {})",
                self.cli_when(r),
                span_words(r - self.as_of)
            ));
        }
        let source = w.source.map_or_else(String::new, |s| {
            format!(
                " ({}, {})",
                s.kind,
                if s.here { "here" } else { "another machine" }
            )
        });
        let read = match w.fresh {
            Fresh::Now => format!("now{source}"),
            Fresh::AsOf => format!("as of {}{source}", self.age(w.at)),
            Fresh::Stale => format!("read {} ago while in use{source}", self.age(w.at)),
            Fresh::IdleHere => format!("as of {}{source}, idle here since", self.age(w.at)),
            Fresh::UsedSince => format!("as of {}{source}, used here since", self.age(w.at)),
            Fresh::Pool => format!("as of {}{source}, others may use it", self.age(w.at)),
            Fresh::Walled => format!(
                "walled to {}",
                row.walled
                    .as_ref()
                    .and_then(|x| x.until)
                    .or(match row.condition {
                        Condition::Limited { until } => Some(until),
                        _ => None,
                    })
                    .map_or_else(|| "its reset".into(), |u| self.cli_when(u))
            ),
            other => other.word().to_string(),
        };
        out.push_str(&format!(" \u{b7} {read}"));
        let refused = match w.window {
            Window::Week => self.demand.week.err(),
            Window::Five => self.demand.five.err(),
        };
        match (refused, w.projection) {
            (Some(why), _) if row.carrier && why != Refusal::Idle => {
                out.push_str(&format!(" \u{b7} not projected: {}", why.words()));
            }
            (None, Some(p)) if p.fills_at.is_some() || p.at_reset_x10 >= 1000 => {
                out.push_str(&format!(
                    " \u{b7} out ~{} before reset",
                    p.fills_at.map_or_else(String::new, |t| {
                        self.cli_when(round_down(t, self.as_of, &self.offsets))
                    })
                ));
            }
            (None, Some(p)) => {
                out.push_str(&format!(" \u{b7} ~{}% by reset", whole(p.at_reset_x10)));
            }
            _ => {}
        }
        out
    }

    /// The CLI's lines under the caller's row line for `label` (§9): one per window and a `plan`
    /// line.
    #[must_use]
    pub fn row_lines(&self, label: &str) -> Vec<String> {
        let Some(row) = self.row(label) else {
            return Vec::new();
        };
        let mut out: Vec<String> = row
            .windows
            .iter()
            .filter(|w| w.window == Window::Week || w.at.is_some())
            .map(|w| self.window_line(row, w))
            .collect();
        // How the replay counts a plan that names no multiple, or any where the carrier's
        // names none (§5.1).
        let counted = if self
            .approx
            .iter()
            .any(|a| matches!(a, Approx::PlansAlike { .. }))
        {
            Some("every row counted alike".to_string())
        } else {
            row.plan.named.is_none().then(|| {
                format!(
                    "counted as {} like {}",
                    self.carrier_plan()
                        .1
                        .unwrap_or_else(|| "the carrier's plan".into()),
                    self.carrier.as_deref().unwrap_or("the carrier")
                )
            })
        };
        let tier = |counted: &Option<String>| {
            counted.as_ref().map_or_else(String::new, |c| {
                if row.plan.named.is_none() {
                    format!("; tier not seen, {c}")
                } else {
                    format!("; {c}")
                }
            })
        };
        let plan = match row.plan.source {
            PlanSource::Assumed => format!(
                "       plan  ? (not seen; {})",
                counted.unwrap_or_else(|| "counted as the carrier's".into())
            ),
            PlanSource::Profile => format!(
                "       plan  {} (profile, {}{})",
                row.plan.title().unwrap_or_default(),
                match row.plan.at {
                    _ if row.carrier => "here".to_string(),
                    Some(at) => format!("read {} ago", span_words(self.as_of - at)),
                    None => "here".to_string(),
                },
                tier(&counted)
            ),
            PlanSource::Renewal => format!(
                "       plan  {} (renewal{})",
                row.plan.title().unwrap_or_default(),
                tier(&counted)
            ),
        };
        out.push(plan);
        out
    }

    /// An assumption's words (`≈` leads them): the CLI's (`card` false, one form), or the
    /// card's, widest first.
    fn approx_words(&self, a: &Approx, card: bool) -> Vec<String> {
        let like = |like: &Option<String>| like.clone().unwrap_or_else(|| "the carrier's".into());
        match a {
            Approx::PlanAssumed {
                label,
                word,
                like: carrier_plan,
                carrier,
            } => {
                let seen = match word {
                    Some(word) => format!(
                        "{}, tier not seen",
                        Plan {
                            m: 1,
                            named: None,
                            word: Some(word.clone()),
                            tier: None,
                            source: PlanSource::Renewal,
                            at: None,
                        }
                        .title()
                        .unwrap_or_default()
                    ),
                    None => "plan not seen".into(),
                };
                let counted = format!("{label}: {seen}, counted as {}", like(carrier_plan));
                if card {
                    vec![counted]
                } else {
                    vec![format!("{counted} like {carrier}")]
                }
            }
            Approx::PlansAlike { carrier } => {
                vec![format!("{carrier}: plan not seen; every row counted alike")]
            }
            Approx::MultiplesScaled if !card => vec![
                "weeks put on one scale by the plans' 5-hour multiples, which Anthropic states \
                 per 5-hour session"
                    .into(),
            ],
            Approx::MultiplesScaled => vec![
                "weeks scaled by the plans' 5-hour multiples".into(),
                "weeks scaled by 5-hour multiples".into(),
            ],
        }
    }

    /// The CLI's `outlook` block (§9).
    #[must_use]
    pub fn text(&self) -> String {
        let mut out = format!(
            "outlook (as of {} \u{b7} rotation {} \u{b7} next 7 days):\n",
            long_when(self.as_of, &self.offsets),
            self.mode.as_str()
        );
        let verdict = match &self.verdict {
            Verdict::Continuous => "continuous".to_string(),
            Verdict::Refused(why) => format!("not projected: {}", why.words()),
            v => words_of(self, v).map_or_else(|| v.kind().to_string(), |w| w.full),
        };
        out.push_str(&format!("  {verdict}\n"));
        let (m, plan) = self.carrier_plan();
        let plan = plan.unwrap_or_else(|| "carrier".into());
        if let (Some(pass), Ok(pace)) = (&self.week, self.demand.week) {
            out.push_str(&format!(
                "  week, at today's pace ({}% of a {plan} week a day, over the last {}):\n",
                self.day_pct().unwrap_or(0),
                lookback_words(pace.span_s)
            ));
            self.pass_lines(pass, &mut out, "    ");
        }
        match (&self.working, self.demand.five) {
            (Some(pass), Ok(pace)) => {
                let hour = u32::try_from((pace.x10_m_per_day / 24 / m).clamp(0, 1000)).unwrap_or(0);
                let head = format!(
                    "  5h, at this hour's pace ({}% of a 5h window an hour), next 12h",
                    whole(hour)
                );
                if pass.stops.is_empty() && pass.lean.is_none() {
                    out.push_str(&format!("{head}: no stop\n"));
                } else {
                    out.push_str(&format!("{head}:\n"));
                    self.pass_lines(pass, &mut out, "    ");
                }
            }
            (_, Err(why)) => out.push_str(&format!("  5h: {}\n", why.words())),
            (None, Ok(_)) => {}
        }
        if !self.not_needed.is_empty() {
            out.push_str(&format!("  not needed: {}\n", self.not_needed.join(", ")));
        }
        for a in &self.approx {
            if let Some(words) = self.approx_words(a, false).into_iter().next() {
                out.push_str(&format!("  \u{2248} {words}\n"));
            }
        }
        out.push_str(
            "  assumes: today's pace holds; a row not read now holds at most what was read, since \
             any Mac may run any account\n",
        );
        out
    }

    fn pass_lines(&self, pass: &Pass, out: &mut String, indent: &str) {
        // A zero-length link (a lean's worst case moving onto a row with nothing) carries nothing.
        let links: Vec<&Link> = pass.chain.iter().filter(|l| l.to > l.from).collect();
        let width = links
            .iter()
            .map(|l| super::footer::text_width(&l.label))
            .max()
            .unwrap_or(4)
            .max(4);
        let at = |t: i64| {
            format!(
                "~{}",
                long_when(round_down(t, self.as_of, &self.offsets), &self.offsets)
            )
        };
        let mut lines: Vec<(i64, String)> = Vec::new();
        for link in links {
            let from = if link.from <= self.as_of {
                "now".to_string()
            } else {
                at(link.from)
            };
            let to = match link.ends {
                Ends::Horizon => "horizon".to_string(),
                _ => at(link.to),
            };
            let of = if pass.kind == PassKind::Working {
                " of its 5h"
            } else {
                ""
            };
            let tail = match link.ends {
                Ends::Leaves => link
                    .left_at_x10
                    .map_or_else(String::new, |x| format!("leaves at {}%{of}", whole(x))),
                Ends::Walls => "walls".into(),
                Ends::Horizon => {
                    let row = self.row(&link.label).and_then(|r| r.window(Window::Week));
                    let reset_inside = row
                        .and_then(|w| w.reset)
                        .filter(|r| *r > link.from && *r <= link.to);
                    match (pass.kind, row.and_then(|w| w.projection), reset_inside) {
                        (PassKind::Week, Some(p), Some(r)) => format!(
                            "~{}% by its reset {}",
                            whole(p.at_reset_x10),
                            long_when(r, &self.offsets)
                        ),
                        _ => link.left_at_x10.map_or_else(String::new, |x| {
                            format!("~{}% used{of} at the horizon", whole(x))
                        }),
                    }
                }
            };
            let pad = width.saturating_sub(super::footer::text_width(&link.label));
            lines.push((
                link.from,
                format!(
                    "{indent}{}{}  {from:<17} \u{2192} {to:<17}  {tail}",
                    link.label,
                    " ".repeat(pad)
                ),
            ));
        }
        for stop in &pass.stops {
            lines.push((
                stop.from,
                format!(
                    "{indent}{:<width$}  {:<17} \u{2192} {}{}",
                    "stop",
                    at(stop.from),
                    match stop.end {
                        StopEnd::Reset(r) => long_when(r, &self.offsets),
                        StopEnd::About(t) =>
                            format!("~{}", long_when(self.end_at(t), &self.offsets)),
                        StopEnd::Open => "no end in reach".into(),
                    },
                    if stop.credits {
                        "  (usage credits would carry it)"
                    } else {
                        ""
                    }
                ),
            ));
        }
        // In time order: a stop sits between the link that walls into it and the one after.
        lines.sort_by_key(|(t, _)| *t);
        for (_, line) in lines {
            out.push_str(line.trim_end());
            out.push('\n');
        }
        if let Some(lean) = &pass.lean {
            out.push_str(&format!(
                "{indent}{:<width$}  {} onto {} ({}):\n",
                "lean",
                at(lean.at),
                lean.row,
                lean.why.word()
            ));
            out.push_str(&format!("{indent}  if it has nothing:\n"));
            self.pass_lines(&lean.worst, out, &format!("{indent}    "));
            out.push_str(&format!(
                "{indent}  if it {}:\n",
                match lean.why {
                    f if f.bounded() => "holds what was read",
                    _ => "has a fresh week",
                }
            ));
            self.pass_lines(&lean.best, out, &format!("{indent}    "));
        }
        if let Some(cut) = pass.cut_at {
            out.push_str(&format!(
                "{indent}(cut at {} after {} events)\n",
                long_when(cut, &self.offsets),
                pass.events
            ));
        }
    }

    /// One window's additive `--json` fields (§9): its `fresh` word and its `projection`.
    #[must_use]
    pub fn window_json(&self, label: &str, window: &str) -> Option<(Value, Value)> {
        let w = match window {
            accounts::WEEK_LIMIT => Window::Week,
            accounts::SESSION_LIMIT => Window::Five,
            _ => return None,
        };
        let row = self.row(label)?;
        let win = row.window(w)?;
        let refused = match w {
            Window::Week => self.demand.week.err(),
            Window::Five => self.demand.five.err(),
        };
        let projection = match (refused, win.projection) {
            (Some(why), _) if row.carrier => {
                let mut o = Map::new();
                o.insert("refused".into(), Value::from(why.word()));
                Value::Object(o)
            }
            (None, Some(p)) => proj_json(p),
            _ => Value::Null,
        };
        Some((Value::from(win.fresh.word()), projection))
    }

    /// A row's additive `plan` field (§9).
    #[must_use]
    pub fn plan_json(&self, label: &str) -> Option<Value> {
        let row = self.row(label)?;
        let mut o = Map::new();
        o.insert(
            "word".into(),
            row.plan.word.clone().map_or(Value::Null, Value::from),
        );
        o.insert(
            "tier".into(),
            row.plan.tier.clone().map_or(Value::Null, Value::from),
        );
        o.insert("m".into(), Value::from(row.plan.m));
        o.insert(
            "named".into(),
            row.plan.named.map_or(Value::Null, Value::from),
        );
        o.insert("source".into(), Value::from(row.plan.source.word()));
        o.insert("at".into(), row.plan.at.map_or(Value::Null, Value::from));
        Some(Value::Object(o))
    }

    /// The top-level `outlook` object of `--json` (§9).
    #[must_use]
    pub fn json(&self) -> Value {
        let mut o = Map::new();
        o.insert("v".into(), Value::from(1u64));
        o.insert("as_of".into(), Value::from(self.as_of));
        o.insert("mode".into(), Value::from(self.mode.as_str()));
        o.insert("credits".into(), Value::from(self.credits));
        o.insert(
            "carrier".into(),
            self.carrier.clone().map_or(Value::Null, Value::from),
        );
        let pace = |p: Result<Pace, Refusal>| {
            let mut o = Map::new();
            match p {
                Ok(p) => {
                    o.insert("x10_m_per_day".into(), Value::from(p.x10_m_per_day));
                    o.insert("basis".into(), Value::from(p.basis.word()));
                    o.insert("span_s".into(), Value::from(p.span_s));
                }
                Err(why) => {
                    o.insert("refused".into(), Value::from(why.word()));
                }
            }
            Value::Object(o)
        };
        let mut demand = Map::new();
        demand.insert("seven_day".into(), pace(self.demand.week));
        demand.insert("five_hour".into(), pace(self.demand.five));
        o.insert("demand".into(), Value::Object(demand));
        let mut v = Map::new();
        v.insert("kind".into(), Value::from(self.verdict.kind()));
        let (from, to, row, why) = match &self.verdict {
            Verdict::Stop { from, lean, .. } => (Some(*from), None, lean.clone(), None),
            Verdict::Lean {
                until, row, why, ..
            } => (None, Some(*until), Some(row.clone()), Some(why.word())),
            Verdict::Walled { to, until } => (None, *until, to.clone(), None),
            Verdict::Credits { at } => (Some(*at), None, None, None),
            Verdict::Refused(why) => (None, None, None, Some(why.word())),
            Verdict::Continuous => (None, None, None, None),
        };
        v.insert("from".into(), from.map_or(Value::Null, Value::from));
        v.insert("to".into(), to.map_or(Value::Null, Value::from));
        v.insert(
            "end".into(),
            match &self.verdict {
                Verdict::Stop { end, .. } => end_json(*end),
                _ => Value::Null,
            },
        );
        v.insert("row".into(), row.map_or(Value::Null, Value::from));
        v.insert("why".into(), why.map_or(Value::Null, Value::from));
        o.insert("verdict".into(), Value::Object(v));
        let passes: Vec<Value> = self
            .week
            .iter()
            .chain(self.working.iter())
            .map(pass_json)
            .collect();
        o.insert("passes".into(), Value::Array(passes));
        let approx: Vec<Value> = self
            .approx
            .iter()
            .map(|a| {
                let mut o = Map::new();
                match a {
                    Approx::PlanAssumed {
                        label,
                        word,
                        like,
                        carrier,
                    } => {
                        o.insert("kind".into(), Value::from("plan-assumed"));
                        o.insert("label".into(), Value::from(label.clone()));
                        o.insert("word".into(), word.clone().map_or(Value::Null, Value::from));
                        o.insert("like".into(), like.clone().map_or(Value::Null, Value::from));
                        o.insert("carrier".into(), Value::from(carrier.clone()));
                    }
                    Approx::PlansAlike { carrier } => {
                        o.insert("kind".into(), Value::from("plans-alike"));
                        o.insert("carrier".into(), Value::from(carrier.clone()));
                    }
                    Approx::MultiplesScaled => {
                        o.insert("kind".into(), Value::from("multiples-scaled"));
                    }
                }
                Value::Object(o)
            })
            .collect();
        o.insert("approx".into(), Value::Array(approx));
        o.insert(
            "not_needed".into(),
            Value::Array(self.not_needed.iter().cloned().map(Value::from).collect()),
        );
        Value::Object(o)
    }
}

/// A pace's lookback as the CLI says it: whole hours up to a day (`24h`), else [`span_words`].
fn lookback_words(span: i64) -> String {
    if span > 0 && span <= DAY && span % HOUR == 0 {
        format!("{}h", span / HOUR)
    } else {
        span_words(span)
    }
}

fn proj_json(p: Proj) -> Value {
    let mut o = Map::new();
    o.insert("at_reset_x10".into(), Value::from(p.at_reset_x10));
    o.insert(
        "fills_at".into(),
        p.fills_at.map_or(Value::Null, Value::from),
    );
    o.insert("pass".into(), Value::from(p.pass.word()));
    Value::Object(o)
}

/// A stop's end as the CLI writes it: `{"kind": "reset"|"about"|"open", "at": t|null}`.
fn end_json(end: StopEnd) -> Value {
    let mut o = Map::new();
    o.insert("kind".into(), Value::from(end.kind()));
    o.insert("at".into(), end.at().map_or(Value::Null, Value::from));
    Value::Object(o)
}

fn pass_json(p: &Pass) -> Value {
    let mut o = Map::new();
    o.insert("pass".into(), Value::from(p.kind.word()));
    o.insert("horizon".into(), Value::from(p.horizon));
    o.insert("cut_at".into(), p.cut_at.map_or(Value::Null, Value::from));
    let chain: Vec<Value> = p
        .chain
        .iter()
        .map(|l| {
            let mut o = Map::new();
            o.insert("label".into(), Value::from(l.label.clone()));
            o.insert("from".into(), Value::from(l.from));
            o.insert("to".into(), Value::from(l.to));
            o.insert("ends".into(), Value::from(l.ends.word()));
            Value::Object(o)
        })
        .collect();
    o.insert("chain".into(), Value::Array(chain));
    let stops: Vec<Value> = p
        .stops
        .iter()
        .map(|s| {
            let mut o = Map::new();
            o.insert("from".into(), Value::from(s.from));
            o.insert("end".into(), end_json(s.end));
            o.insert("window".into(), Value::from(s.window.id()));
            o.insert("credits".into(), Value::from(s.credits));
            Value::Object(o)
        })
        .collect();
    o.insert("stops".into(), Value::Array(stops));
    o.insert(
        "lean".into(),
        p.lean.as_ref().map_or(Value::Null, |l| {
            let mut o = Map::new();
            o.insert("row".into(), Value::from(l.row.clone()));
            o.insert("at".into(), Value::from(l.at));
            o.insert("why".into(), Value::from(l.why.word()));
            o.insert("worst".into(), pass_json(&l.worst));
            o.insert("best".into(), pass_json(&l.best));
            Value::Object(o)
        }),
    );
    Value::Object(o)
}

#[cfg(test)]
#[path = "outlook_tests.rs"]
pub(crate) mod tests;
