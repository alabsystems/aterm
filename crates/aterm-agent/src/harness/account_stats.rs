// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Per-machine Claude Code account stats view ([`Missing`], [`super::accounts::State`],
//! [`Account::windows`], [`Account::offer`], [`accounts::sample_window`], [`super::profile`],
//! [`super::spend`], [`super::accounts::login_state`], [`merge_fig`]).

use std::fmt::Write as _;

use aterm_json::{Map, Value};
use aterm_phase::ResetKind;

use super::account_series::{self, Moved, SeriesRow, Since};
use super::accounts::{
    self, Account, Credits, Offer, SESSION_LIMIT, SESSION_WINDOW_S, SOURCE_SAMPLE, WEEK_LIMIT,
    WEEK_WINDOW_S, WeekSaid, WindowFig, WindowSaid, merge_fig,
};
use super::profile::{self, Bucket, NoProfile, Profile};
use super::spend::{self, Spend};
use super::usage::rfc3339_utc;

/// [`WindowFig::source`] of a figure the home's cached `/usage` body held.
pub const SOURCE_USAGE_CACHE: &str = "usage-cache";

/// [`WindowFig::source`] of a usage wall.
pub const SOURCE_WALL: &str = "wall";

/// [`WindowFig::source`] of Claude Code's warning that a window is filling.
pub const SOURCE_WARNING: &str = "warning";

/// The `resets` figure's source: the harness's limit reset, which records the offer from an exact
/// confirmation or a spent line read whole ([`accounts::note_offer`], the resets spec's R22).
pub const SOURCE_LIMIT_RESET: &str = "limit-reset";

/// THE MODEL FAMILY A PER-MODEL WEEK'S DOLLARS ARE SUMMED FOR
/// ([`spend::Spend::sum`], the catalog family of each message's model):
/// the Fable week's every Fable model's, the Sonnet and Opus weeks' their
/// own.
fn week_family(id: &str) -> Option<&'static str> {
    [
        (Bucket::Fable, "fable"),
        (Bucket::SevenDaySonnet, "sonnet"),
        (Bucket::SevenDayOpus, "opus"),
    ]
    .into_iter()
    .find_map(|(bucket, family)| (bucket.id() == id).then_some(family))
}

/// Whether the usage tap can have read this home's windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tap {
    /// `[harness] usage_tap = false` (it ships `true`).
    Off,
    /// `usage_tap` on, and the harness off for Claude Code (`[harness] enabled = false` or
    /// `[harness.claude] enabled = false`), so no relaunch carries the tap: the windows' closed
    /// word is still `usage-tap-off`, and the text names the harness, not the key.
    HarnessOff,
    /// On.
    On,
    /// On, but no tap reading is recorded under the label.
    OtherHome,
}

/// Why a figure is empty, one closed word each ([`Missing::word`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Missing {
    /// The tap is off and nothing else read the window.
    UsageTapOff,
    /// No tap reading is recorded under the label ([`Tap::OtherHome`]).
    TapOtherHome,
    /// The tap is on and has recorded nothing of the window.
    NoReading,
    /// The five-hour window only: the usage tap's newest sample of the account carried the week
    /// and no five-hour window. Claude Code hands a window only while the API reports it
    /// (2.1.316's statusLine `rate_limits`), so the vendor reports none for this login now
    /// ([`not_reported`]).
    NotReported,
    /// The window's reset passed with no reading.
    WindowReset,
    /// A limit reset was used inside the window, with no reading since.
    ResetUsed,
    /// No `/usage` was seen on this home.
    NoUsageSeen,
    /// The usage body this home cached has no row for the window.
    NoRow,
    /// The profile could not be read ([`NoProfile`]).
    Profile(NoProfile),
    /// The accounts state could not be read.
    StateUnread,
    /// The `resets` figure only: the harness's limit reset is on ([`Look::limit_reset`]) and the
    /// accounts state holds no offer of the account ([`Account::offer`]): the program has read no
    /// exact confirmation and no spent line of it (the resets spec's R22).
    NoGrantRead,
    /// The `resets` figure only: no offer is held and the harness's limit reset, the one program
    /// that records one ([`accounts::note_offer`]), is off ([`Look::limit_reset`]). A reset a person
    /// takes records none.
    LimitResetOff,
}

impl Missing {
    /// The closed word.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Missing::UsageTapOff => "usage-tap-off",
            Missing::TapOtherHome => "tap-not-this-home",
            Missing::NoReading => "no-reading",
            Missing::NotReported => "not-reported",
            Missing::WindowReset => "window-reset",
            Missing::ResetUsed => "reset-used",
            Missing::NoUsageSeen => "no-usage-seen",
            Missing::NoRow => "no-row",
            Missing::Profile(why) => why.word(),
            Missing::StateUnread => "state-unread",
            Missing::NoGrantRead => "no-grant-read",
            Missing::LimitResetOff => "limit-reset-off",
        }
    }

    /// The reason in a few words, for a one-line figure that has none (`week – (no reading
    /// yet)`): never a bare `?`.
    #[must_use]
    pub const fn phrase(self) -> &'static str {
        match self {
            Missing::UsageTapOff => "usage tap off",
            Missing::TapOtherHome => "no tap reading of this home",
            Missing::NoReading => "no reading yet",
            Missing::NotReported => "not reported by Claude Code",
            Missing::WindowReset => "window reset, no reading since",
            Missing::ResetUsed => "a limit reset since, no reading since",
            Missing::NoUsageSeen => "no /usage seen on this home",
            Missing::NoRow => "no row in the /usage seen",
            Missing::Profile(_) => "the profile could not be read",
            Missing::StateUnread => "the accounts state could not be read",
            Missing::NoGrantRead => "no offer read yet",
            Missing::LimitResetOff => {
                "not read: only the harness's limit reset ([harness] limit_reset) records an \
                 offer, and it is off"
            }
        }
    }
}

/// S7's word on the login, where the label's roster row is renewable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Login {
    /// `ok`, `due`, `expired` or `idle` ([`super::accounts::LoginState`]).
    pub state: &'static str,
    /// Renew by this instant, where a forecast names one.
    pub renew_by: Option<i64>,
}

/// The home's spend, folded by the caller.
#[derive(Debug, Clone, Copy)]
pub struct SpendLook<'a> {
    /// The buckets.
    pub spend: &'a Spend,
    /// Whether the fold reached the end of every transcript in time.
    pub complete: bool,
}

/// Everything the view is built from, gathered by the caller.
#[derive(Debug, Clone, Copy)]
pub struct Look<'a> {
    /// Unix seconds.
    pub now: i64,
    /// The local clock's offset from UTC, seconds.
    pub tz_s: i64,
    /// This machine's kept id ([`super::accounts::Store::machine_id`]).
    pub machine: Option<&'a str>,
    /// The selected home's display label.
    pub label: &'a str,
    /// The home's profile, as the allowlist read it.
    pub profile: &'a Result<Profile, NoProfile>,
    /// What the accounts state holds of the label.
    pub account: Option<&'a Account>,
    /// The accounts state could not be read.
    pub state_unread: bool,
    /// The roster lists several accounts, every one a login of the default home.
    pub several: bool,
    /// A Claude Code running on the home now is not on its stored login.
    pub api_key_seen: bool,
    /// [`Tap`].
    pub tap: Tap,
    /// Whether the harness's limit reset is on for Claude Code
    /// (`SupervisorConfig::limit_reset_on`): the one program that records a limit-reset offer
    /// ([`accounts::note_offer`]), so with it off and no offer held the `resets` figure says
    /// [`Missing::LimitResetOff`].
    pub limit_reset: bool,
    /// [`SpendLook`]; `None` where no Claude directory is known.
    pub spend: Option<SpendLook<'a>>,
    /// The price table's date.
    pub prices_as_of: Option<&'a str>,
    /// [`Login`], where the label's row is renewable.
    pub login: Option<Login>,
    /// The Claude Codes running on this home now, or why they were not counted.
    pub sessions: Result<usize, &'a str>,
}

impl<'a> Look<'a> {
    /// THE RECORD THE VIEW READS: what the accounts state holds of the
    /// label, unless it is another account's than the one the profile names
    /// ([`Account::of_another`], `note_reading`'s own rule).
    fn record(&self) -> Option<&'a Account> {
        self.account.filter(|a| !a.of_another(self.key()))
    }

    /// The key of the account the home's profile currently names, where it
    /// was read.
    fn key(&self) -> Option<profile::AccountKey> {
        self.profile.as_ref().ok().and_then(|p| p.key)
    }

    /// [`another_seen`] on the home this view is of.
    fn another_seen(&self) -> Option<i64> {
        another_seen(
            self.now,
            self.several,
            self.account,
            self.profile.as_ref().ok(),
        )
    }

    /// WHOSE THE HOME'S SPEND FROM `from` IS (design §3.6): ASSUMED to be
    /// the current account's where no other account was seen on the home
    /// since `from` ([`Self::another_seen`]); else UNATTRIBUTED, all of it
    /// (no share of it is split off by time), so no capacity and no `left`
    /// is claimed from it.
    fn attribution(&self, from: i64) -> spend::Attribution {
        if self.another_seen().is_some_and(|at| at >= from) {
            spend::Attribution::Unattributed { share: 1.0 }
        } else {
            spend::Attribution::Assumed
        }
    }
}

/// THE NEWEST INSTANT ANOTHER ACCOUNT THAN THE ONE THE PROFILE NAMES WAS SEEN ON THE HOME
/// (design §3.6), at `now`, of the home's label's record `account` and the home's `profile`
/// where it was read: `now` where the home serves several accounts' logins (`several`) or the
/// record is another account's ([`Account::of_another`]: its observations were of the account
/// signed in before, since when is not known); the instant the record's key changed
/// ([`Account::keyed_at`], the `epoch` row's); the fetch of a cached `/usage` body that named
/// another account ([`Profile::usage_of_another_at`]). What the home did after it is ASSUMED to
/// be the current account's: the spend attribution's rule ([`Look`]) and the account backfill's
/// (`super::account_backfill`).
pub(crate) fn another_seen(
    now: i64,
    several: bool,
    account: Option<&Account>,
    profile: Option<&Profile>,
) -> Option<i64> {
    let key = profile.and_then(|p| p.key);
    let another = account.is_some_and(|a| a.of_another(key));
    let held = (several || another).then_some(now);
    let keyed = account.filter(|_| !another).and_then(|a| a.keyed_at);
    [held, keyed, profile.and_then(|p| p.usage_of_another_at)]
        .into_iter()
        .flatten()
        .max()
}

/// One window as the view has it.
#[derive(Debug, Clone, PartialEq)]
pub struct Win {
    /// The closed bucket id ([`Bucket::id`]).
    pub id: &'static str,
    /// `five_hour`, `seven_day` or `model_weekly`.
    pub kind: &'static str,
    /// A per-model week's model name (`Fable`).
    pub name: Option<String>,
    /// Its length, seconds.
    pub length_s: i64,
    /// The figure, current or void or cut by a reset used inside it.
    pub fig: Option<WindowFig>,
    /// WHERE THE FIGURE NAMED NO RESET (a wall that gave none: `100%, no reset given`, the stats
    /// design's Q11), the instant it is void at: the reset an older reading of the same week named
    /// after it, and never later than a week after the reading, since no week outlasts its length
    /// ([`unnamed_week`]). The window has reset by then; the figure's `reset` holds the same
    /// instant, so [`WindowFig::current`] reads it, and the window's own reset is unknown
    /// ([`Self::resets_at`]).
    pub void_at: Option<i64>,
    /// Why it says nothing now.
    pub missing: Option<Missing>,
    /// How far its figure moved over the asked span, where the readings series says
    /// ([`with_since`], [`account_series::since_of`]).
    pub since: Option<Since>,
}

impl Win {
    /// The figure, where it is current and not cut.
    #[must_use]
    pub fn current(&self, now: i64) -> Option<&WindowFig> {
        self.fig.as_ref().filter(|f| f.current(now) && !f.cut())
    }

    /// The window's reset, where its figure named one.
    #[must_use]
    pub fn resets_at(&self) -> Option<i64> {
        self.fig
            .as_ref()
            .filter(|_| self.void_at.is_none())
            .map(|f| f.reset)
    }
}

/// The view of one account.
#[derive(Debug, Clone, PartialEq)]
pub struct View {
    /// The five-hour window, the week, the Fable week.
    pub windows: Vec<Win>,
    /// A non-zero tenth was seen in the account's figures.
    pub tenths: bool,
}

/// A figure of one window, at its source.
fn fig(pct_x10: u32, reset: i64, at: i64, by: &str, source: &str) -> WindowFig {
    WindowFig {
        pct_x10,
        reset,
        at,
        by: by.to_owned(),
        source: source.to_owned(),
        epoch_at: None,
    }
}

/// THE ONE FIGURE OF A WINDOW FROM ALL ITS SOURCES: current
/// figures merged by [`merge_fig`]; with none current, the newest void one.
fn one_figure(
    sources: impl IntoIterator<Item = WindowFig>,
    refilled: Option<i64>,
    length_s: i64,
    now: i64,
) -> Option<WindowFig> {
    let mut current: Option<WindowFig> = None;
    let mut void: Option<WindowFig> = None;
    for mut f in sources {
        if f.current(now) {
            if let Some(at) = refilled.filter(|at| *at >= f.reset.saturating_sub(length_s)) {
                f.epoch_at = f.epoch_at.max(Some(at));
            }
            current = merge_fig(current.as_ref(), Some(&f), now);
        } else if void
            .as_ref()
            .is_none_or(|v| (f.reset, f.at) > (v.reset, v.at))
        {
            void = Some(f);
        }
    }
    current.or(void)
}

/// The figure S5's word on a window says ([`WindowSaid`]): a warning's
/// percent until its named reset; `None` for a refill (no figure) or a
/// warning that named no reset.
fn said_fig(said: &WindowSaid, by: &str) -> Option<WindowFig> {
    Some(fig(
        said.pct?.saturating_mul(10),
        said.reset?,
        said.at,
        by,
        SOURCE_WARNING,
    ))
}

/// The instant S5's word says a limit reset was USED inside the window: a
/// word with neither a figure nor a reset.
fn said_refill(said: Option<&WindowSaid>) -> Option<i64> {
    said.filter(|s| s.pct.is_none() && s.reset.is_none())
        .map(|s| s.at)
}

/// The source word of a model's week a screen said: its wall's or its warning's.
const fn week_source(said: &WeekSaid) -> &'static str {
    if said.wall {
        SOURCE_WALL
    } else {
        SOURCE_WARNING
    }
}

/// The figure a screen's word on a model's week says where it named the week's reset.
fn week_said_fig(said: &WeekSaid) -> Option<WindowFig> {
    Some(fig(
        said.pct?.saturating_mul(10),
        said.reset?,
        said.at,
        &said.by,
        week_source(said),
    ))
}

/// The instant a screen's word says a limit reset was USED on a model's week
/// ([`accounts::reset_outcome`]), as [`said_refill`] reads the session's and the week's.
fn week_refill(said: Option<&WeekSaid>) -> Option<i64> {
    said.filter(|s| s.pct.is_none() && s.reset.is_none())
        .map(|s| s.at)
}

/// A MODEL'S WEEK READ WITH NO RESET (the stats design's Q11), where it is the newest word on the
/// week: the newer of the screen's wall that named none (`said`, [`Account::weeks_said`]; a
/// warning that named none is no figure, as a session or weekly one is not, [`said_fig`]) and a
/// wall recorded with none (`wall`, a 429 that named no reset), unless a figure of the same week
/// (`sources`) or a limit reset on it ([`week_refill`]'s word, or a lift's) came with it or after
/// it. It is the figure until it is void ([`Win::void_at`]): at the reset an older reading of that
/// week named after it (a figure of `sources`, or the screen's [`WeekSaid::ends_by`]), since the
/// week that held both readings ends there, and never later than a week after it, since no week
/// outlasts its length. Another week's reset (the week of all models) is another window's and
/// ends nothing here. A merge cannot hold it ([`merge_fig`]): an invented reset would be a window
/// of its own.
fn unnamed_week(
    said: Option<&WeekSaid>,
    wall: Option<&accounts::Wall>,
    sources: &[WindowFig],
) -> Option<(WindowFig, i64)> {
    let reset_word = said.filter(|w| w.pct.is_none()).map(|w| w.at);
    let said = said.filter(|w| w.wall && w.reset.is_none());
    let read = said.and_then(|w| Some((w.pct?.saturating_mul(10), w.at, w.by.as_str())));
    let wall = wall
        .filter(|w| !w.cleared && w.until.is_none())
        .map(|w| (1000, w.at, w.by.as_str()));
    let (pct_x10, at, by) = [wall, read]
        .into_iter()
        .flatten()
        .max_by_key(|(_, at, _)| *at)?;
    if sources
        .iter()
        .map(|f| f.at)
        .chain(reset_word)
        .any(|t| t >= at)
    {
        return None;
    }
    let void_at = sources
        .iter()
        .map(|f| f.reset)
        .chain(said.and_then(|w| w.ends_by))
        .filter(|end| *end > at)
        .chain([at.saturating_add(WEEK_WINDOW_S)])
        .min()?;
    Some((fig(pct_x10, void_at, at, by, SOURCE_WALL), void_at))
}

/// WHETHER CLAUDE CODE REPORTS NO FIVE-HOUR FIGURE FOR `account` NOW ([`Missing::NotReported`];
/// `id` is [`SESSION_LIMIT`], any other window is never said so): its newest sample, published
/// after the five-hour window's held figure `held` reset (where one is held), carried the week and
/// no five-hour window, and that week is the usage tap's own reading
/// (`windows["seven_day"]`, observed no earlier than the sample's week). A tap reading writes a
/// sample only where it carried a window, holding each window it carried merged with what the
/// record held live ([`accounts::note_reading`]), so the statusLine that brought the week brought
/// no five-hour window and none was held. A `/usage` panel's sample is not taken for this: a row
/// it lacks may be one a narrow pane cut.
fn not_reported(account: Option<&Account>, id: &str, held: Option<&WindowFig>) -> bool {
    let Some((account, sample)) = account.and_then(|a| Some((a, a.sample.as_ref()?))) else {
        return false;
    };
    let tapped_week = account.windows.get(WEEK_LIMIT).is_some_and(|week| {
        week.source == accounts::SOURCE_TAP && week.at >= sample.week_read_at.unwrap_or(sample.at)
    });
    id == SESSION_LIMIT
        && sample.session_pct.is_none()
        && sample.week_pct.is_some()
        && tapped_week
        && held.is_none_or(|f| sample.at >= f.reset)
}

/// The view of `look` (module docs).
#[must_use]
pub fn view(look: &Look<'_>) -> View {
    let now = look.now;
    let here = look.machine.unwrap_or_default();
    let account = look.record();
    let cache = look.profile.as_ref().ok().and_then(|p| p.usage.as_ref());
    let cache_at = cache.map(|c| c.fetched_at_ms.div_euclid(1000));
    let cached = |bucket: Bucket, name: Option<&str>| -> Vec<&profile::CachedWindow> {
        cache
            .into_iter()
            .flat_map(|c| c.windows.iter())
            .filter(|w| {
                w.bucket == bucket
                    && name.is_none_or(|n| w.name.as_ref().is_some_and(|m| m.as_str() == n))
            })
            .collect()
    };
    let cached_figs = |bucket: Bucket, name: Option<&str>| {
        cached(bucket, name)
            .into_iter()
            .filter_map(|w| {
                Some(fig(
                    u32::from(w.used_pct_x10?),
                    w.resets_at?,
                    cache_at?,
                    here,
                    SOURCE_USAGE_CACHE,
                ))
            })
            .collect::<Vec<_>>()
    };
    let held = |id: &str| {
        let Some(a) = account else {
            return Vec::new();
        };
        let wall = a
            .walls
            .get(id)
            .filter(|w| !w.cleared)
            .and_then(|w| Some(fig(1000, w.until?, w.at, &w.by, SOURCE_WALL)));
        let stored = a.windows.get(id);
        stored
            .cloned()
            .into_iter()
            .chain(accounts::sample_window(a.sample.as_ref(), id, stored))
            .chain(wall)
            .collect::<Vec<_>>()
    };
    let tap_missing = || {
        if look.state_unread {
            Missing::StateUnread
        } else {
            match look.tap {
                Tap::Off | Tap::HarnessOff => Missing::UsageTapOff,
                Tap::OtherHome => Missing::TapOtherHome,
                Tap::On => Missing::NoReading,
            }
        }
    };
    let cache_missing = || match look.profile {
        Err(why) => Missing::Profile(*why),
        Ok(p) if p.usage.is_none() => Missing::NoUsageSeen,
        Ok(_) => Missing::NoRow,
    };
    let state_of = |f: &Option<WindowFig>, none: Missing| match f {
        None => Some(none),
        Some(f) if !f.current(now) => Some(Missing::WindowReset),
        Some(f) if f.cut() => Some(Missing::ResetUsed),
        Some(_) => None,
    };
    let mut windows = Vec::new();
    for (id, bucket, length_s, said) in [
        (
            SESSION_LIMIT,
            Bucket::FiveHour,
            SESSION_WINDOW_S,
            account.and_then(|a| a.warned_session.as_ref()),
        ),
        (
            WEEK_LIMIT,
            Bucket::SevenDay,
            WEEK_WINDOW_S,
            account.and_then(|a| a.warned_week.as_ref()),
        ),
    ] {
        let sources = held(id)
            .into_iter()
            .chain(cached_figs(bucket, None))
            .chain(said.and_then(|s| said_fig(s, here)));
        let f = one_figure(sources, said_refill(said), length_s, now);
        let missing = match state_of(&f, tap_missing()) {
            Some(Missing::NoReading | Missing::WindowReset)
                if not_reported(account, id, f.as_ref()) =>
            {
                Some(Missing::NotReported)
            }
            other => other,
        };
        windows.push(Win {
            id,
            kind: id,
            name: None,
            length_s,
            missing,
            fig: f,
            void_at: None,
            since: None,
        });
    }
    let mut weeks = vec![
        (Bucket::Fable, None),
        (Bucket::SevenDaySonnet, None),
        (Bucket::SevenDayOpus, None),
    ];
    for name in cached(Bucket::Other, None)
        .into_iter()
        .filter_map(|w| w.name.as_ref())
    {
        let week = (Bucket::Other, Some(name.as_str()));
        if !weeks.contains(&week) {
            weeks.push(week);
        }
    }
    for (bucket, model) in weeks {
        let name = cached(bucket, model)
            .into_iter()
            .find_map(|w| w.name.as_ref())
            .map(|n| n.as_str().to_owned());
        let said = account
            .filter(|_| model.is_none())
            .and_then(|a| a.weeks_said.get(bucket.id()));
        let sources: Vec<WindowFig> = held(bucket.id())
            .into_iter()
            .chain(cached_figs(bucket, model))
            .chain(said.and_then(week_said_fig))
            .collect();
        let wall = account.and_then(|a| a.walls.get(bucket.id()));
        let (f, void_at) = match unnamed_week(said, wall, &sources) {
            Some((f, void_at)) => (Some(f), Some(void_at)),
            None => (
                one_figure(sources, week_refill(said), WEEK_WINDOW_S, now),
                None,
            ),
        };
        if bucket != Bucket::Fable && f.is_none() {
            continue;
        }
        windows.push(Win {
            id: bucket.id(),
            kind: "model_weekly",
            name: name.or_else(|| (bucket == Bucket::Fable).then(|| "Fable".to_owned())),
            length_s: WEEK_WINDOW_S,
            missing: state_of(&f, cache_missing()),
            fig: f,
            void_at,
            since: None,
        });
    }
    let tenths = windows
        .iter()
        .filter_map(|w| w.fig.as_ref())
        .any(|f| f.pct_x10 % 10 != 0);
    View { windows, tenths }
}

/// Fills each window's [`Win::since`] from `rows`, the label's readings series, for the account
/// its record's `keys` name: how far its current figure moved over the last `span_s` before
/// `now` ([`account_series::since_of`]). The one filler, so `accounts stats` and the accounts
/// report say one `since`. A caller whose record is another account's passes no rows.
pub fn with_since(
    view: &mut View,
    (rows, keys): (&[SeriesRow], account_series::Keys<'_>),
    span_s: i64,
    now: i64,
) {
    for win in &mut view.windows {
        win.since = account_series::since_of(
            rows,
            keys,
            (win.id, win.length_s),
            win.current(now),
            span_s,
            now,
        );
    }
}

/// The two windows the accounts state holds of `account`.
#[must_use]
pub fn held_windows(
    account: &Account,
    key: Option<profile::AccountKey>,
    machine: Option<&str>,
    tap: Tap,
    now: i64,
) -> View {
    let none = Err(NoProfile::NoHome);
    home_windows(
        Some(account).filter(|a| !a.of_another(key)),
        &none,
        machine,
        tap,
        now,
    )
}

/// THE TWO WINDOWS aterm HOLDS OF `account` ON A HOME WHOSE PROFILE READ IS `profile`: the
/// accounts state's figures (every tapped session's on its login, its newest sample, a standing
/// wall, Claude Code's warning) and the home's cached `/usage` body, merged as `accounts stats`
/// merges them ([`view`]), each with why it says nothing where it does not ([`Missing`]). The
/// record is dropped where it is another account's than the one the profile names
/// ([`Account::of_another`]).
#[must_use]
pub fn home_windows(
    account: Option<&Account>,
    profile: &Result<Profile, NoProfile>,
    machine: Option<&str>,
    tap: Tap,
    now: i64,
) -> View {
    let look = Look {
        now,
        tz_s: 0,
        machine,
        label: "",
        profile,
        account,
        state_unread: false,
        several: false,
        api_key_seen: false,
        tap,
        limit_reset: false,
        spend: None,
        prices_as_of: None,
        login: None,
        sessions: Err("not-counted"),
    };
    let mut held = view(&look);
    held.windows.retain(|w| w.kind != "model_weekly");
    held
}

/// How a figure's source was read, for its resolution ([`spend::ReadingKind`]).
fn reading_kind(source: &str) -> spend::ReadingKind {
    match source {
        SOURCE_WALL => spend::ReadingKind::Wall,
        SOURCE_SAMPLE => spend::ReadingKind::Floored,
        _ => spend::ReadingKind::Rounded,
    }
}

/// Whether usage credits served a message in the window that began at
/// `from`: S5 read them in use, its newest answer on them inside it.
fn on_credits(account: Option<&Account>, from: i64) -> bool {
    matches!(
        account.and_then(|a| a.credits.as_ref()),
        Some(Credits::InUse(c)) if c.seen >= from
    )
}

/// One window's dollars.
#[must_use]
pub fn dollars(look: &Look<'_>, view: &View, win: &Win) -> Option<spend::Figure> {
    let spend = look.spend?.spend;
    let family = match win.kind {
        "model_weekly" => Some(week_family(win.id)?),
        _ => None,
    };
    // A figure that named no reset gives no window start, so its week is the rolling one.
    let current = win.current(look.now).filter(|_| win.void_at.is_none());
    let reading = current.map(|f| spend::Reading {
        pct_x10: f.pct_x10,
        at: f.at,
        kind: reading_kind(&f.source),
    });
    let resets_at = current.map(|f| f.reset);
    let from = resets_at.map_or(look.now.saturating_sub(win.length_s), |r| {
        r.saturating_sub(win.length_s)
            .max(current.and_then(|f| f.epoch_at).unwrap_or(i64::MIN))
    });
    let ask = spend::Ask {
        length_s: win.length_s,
        resets_at,
        epoch_at: current.and_then(|f| f.epoch_at),
        family,
        reading,
        newest: reading,
        tenths_seen: view.tenths,
        on_credits: on_credits(look.record(), from),
        attribution: look.attribution(from),
        api_key_seen: look.api_key_seen,
        machines: spend::Machines {
            reported: 1,
            used: 1,
            silent: 0,
        },
        now: look.now,
    };
    Some(spend::figure(spend, &ask))
}

const DASH: &str = "\u{2014}";
const DOT: &str = " \u{b7} ";

/// A span of seconds as the text says it: `40s`, `12m`, `1h25m`, `3d4h`.
pub(super) fn span(s: i64) -> String {
    let s = s.max(0);
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m", s / 60),
        3600..86_400 => match (s / 3600, s % 3600 / 60) {
            (h, 0) => format!("{h}h"),
            (h, m) => format!("{h}h{m}m"),
        },
        _ => match (s / 86_400, s % 86_400 / 3600) {
            (d, 0) => format!("{d}d"),
            (d, h) => format!("{d}d{h}h"),
        },
    }
}

const WEEKDAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// The local day of `t` (`tz_s` from UTC) as `Sun Oct 18`.
pub(super) fn day_of(t: i64, tz_s: i64) -> String {
    let day = t.saturating_add(tz_s).div_euclid(86_400);
    let (_, month, d) = aterm_types::rfc3339::civil_from_days(day);
    let weekday = WEEKDAYS
        .get(usize::try_from(day.rem_euclid(7)).unwrap_or(0))
        .copied()
        .unwrap_or("");
    let month = MONTHS
        .get(usize::try_from(month - 1).unwrap_or(0))
        .copied()
        .unwrap_or("");
    format!("{weekday} {month} {d}")
}

/// The local time of day of `t` as `18:05`.
pub(super) fn hm(t: i64, tz_s: i64) -> String {
    let secs = t.saturating_add(tz_s).rem_euclid(86_400);
    format!("{:02}:{:02}", secs / 3600, secs % 3600 / 60)
}

/// An instant on the local clock: `18:05` on the local day of `now`, else
/// `Sun Oct 18 20:50`.
pub(super) fn clock_local(t: i64, now: i64, tz_at: &dyn Fn(i64) -> i64) -> String {
    let (t_tz, now_tz) = (tz_at(t), tz_at(now));
    let same =
        t.saturating_add(t_tz).div_euclid(86_400) == now.saturating_add(now_tz).div_euclid(86_400);
    if same {
        hm(t, t_tz)
    } else {
        format!("{} {}", day_of(t, t_tz), hm(t, t_tz))
    }
}

/// A percent in tenths as the text says it.
pub fn percent(x10: u32, tenths: bool) -> String {
    if tenths {
        format!("{}.{}%", x10 / 10, x10 % 10)
    } else {
        format!("{}%", (x10 + 5) / 10)
    }
}

/// List dollars as the text says them: `$4.21` under ten, `$1,890` from there.
fn usd(v: f64) -> String {
    if !v.is_finite() {
        return DASH.to_owned();
    }
    if v < 10.0 {
        return format!("${v:.2}");
    }
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "rounded, positive, finite"
    )]
    let whole = v.round().max(0.0) as u64;
    let digits = whole.to_string();
    let mut out = String::from("$");
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// A source as the text names it.
fn source_word(source: &str) -> &str {
    match source {
        SOURCE_USAGE_CACHE => "/usage",
        other => other,
    }
}

/// A figure's machine as the text names it: `here`, else its id's first six
/// digits.
fn machine_word<'a>(by: &'a str, look: &Look<'_>) -> &'a str {
    if look.machine == Some(by) || by.is_empty() {
        "here"
    } else {
        by.get(..6).unwrap_or(by)
    }
}

/// The window's row label in the text.
fn row_label(win: &Win) -> String {
    match win.kind {
        "five_hour" => "5h".to_owned(),
        "seven_day" => "week".to_owned(),
        _ => win.name.clone().unwrap_or_else(|| win.id.to_owned()),
    }
}

/// Why a window says nothing, as the text says it.
fn missing_text(win: &Win, look: &Look<'_>, tz_at: &dyn Fn(i64) -> i64) -> String {
    let at = |t: i64| clock_local(t, look.now, tz_at);
    match (win.missing, win.fig.as_ref()) {
        (Some(Missing::WindowReset), Some(f)) if win.void_at.is_some() => {
            format!("window reset by {}, no reading since", at(f.reset))
        }
        (Some(Missing::WindowReset), Some(f)) => {
            format!("window reset {}, no reading since", at(f.reset))
        }
        (Some(Missing::ResetUsed), Some(f)) => format!(
            "reset used {}, no reading since",
            at(f.epoch_at.unwrap_or(f.at))
        ),
        (Some(Missing::UsageTapOff), _)
            if win.kind == "five_hour" && look.tap == Tap::HarnessOff =>
        {
            "usage_tap off (the harness is off for Claude Code)".to_owned()
        }
        (Some(Missing::UsageTapOff), _) if win.kind == "five_hour" => {
            "usage_tap off ([harness] usage_tap = true turns it on)".to_owned()
        }
        (Some(Missing::UsageTapOff), _) => "usage_tap off".to_owned(),
        (Some(Missing::TapOtherHome), _) => {
            "the usage tap records no reading of this login: it is no row of a roster that admits"
                .to_owned()
        }
        (Some(Missing::NoReading), _) => "no reading yet".to_owned(),
        (Some(Missing::NotReported), _) => {
            let age = look
                .record()
                .and_then(|a| a.sample.as_ref())
                .map_or_else(String::new, |s| {
                    format!(" (as of {})", span(look.now.saturating_sub(s.at)))
                });
            format!(
                "not reported: Claude Code reports the week but no 5h window for this login{age}"
            )
        }
        (Some(Missing::NoUsageSeen), _) => "no /usage seen on this home".to_owned(),
        (Some(Missing::NoRow), _) => "the /usage seen on this home has no row for it".to_owned(),
        (Some(Missing::StateUnread), _) => "the accounts state could not be read".to_owned(),
        (Some(Missing::Profile(why)), _) => format!("profile: {}", why.word()),
        (Some(m @ (Missing::NoGrantRead | Missing::LimitResetOff)), _) => m.phrase().to_owned(),
        (None, _) => String::new(),
        (Some(other), None) => other.word().to_owned(),
    }
}

/// The plan's answer on Fable, as the text says it.
fn fable_plan_text(p: &Profile, tz_at: &dyn Fn(i64) -> i64) -> String {
    match &p.fable {
        profile::FablePlan::Included => "Fable included".to_owned(),
        profile::FablePlan::CreditsOnly => "credits only".to_owned(),
        profile::FablePlan::LimitsEnd(Some(at)) => {
            format!("limits end {}", day_of(*at, tz_at(*at)))
        }
        profile::FablePlan::LimitsEnd(None) => "limits end (date unread)".to_owned(),
        profile::FablePlan::Unknown => "unknown".to_owned(),
    }
}

/// A span the way `since` names it: whole hours up to a day as hours (`24h`, `3h`), else as
/// [`span`] says it (`7d`, `3h12m`).
pub(super) fn since_span(s: i64) -> String {
    if s > 0 && s <= 86_400 && s % 3600 == 0 {
        format!("{}h", s / 3600)
    } else {
        span(s)
    }
}

/// A window's `since` as its row says it: `+18 since 24h`, `new window since 13:00` or `reset
/// used since 13:00`.
fn since_text(since: &Since, now: i64, tz_at: &dyn Fn(i64) -> i64) -> String {
    match since.moved {
        Moved::Rose(points) => format!("+{points} since {}", since_span(since.span_s)),
        Moved::NewWindow(at) => format!("new window since {}", clock_local(at, now, tz_at)),
        Moved::ResetUsed(at) => format!("reset used since {}", clock_local(at, now, tz_at)),
    }
}

/// One window's row.
fn window_row(
    look: &Look<'_>,
    view: &View,
    win: &Win,
    p: Option<&Profile>,
    tz_at: &dyn Fn(i64) -> i64,
) -> String {
    let now = look.now;
    let label = row_label(win);
    let mut row = match win.current(now) {
        Some(f) if win.void_at.is_some() => format!(
            "{}, no reset given{DOT}as of {} ({}, {})",
            percent(f.pct_x10, view.tenths),
            span(now.saturating_sub(f.at)),
            source_word(&f.source),
            machine_word(&f.by, look)
        ),
        Some(f) => {
            let left = 1000u32.saturating_sub(f.pct_x10);
            let mut row = format!(
                "{} used{DOT}{} left{DOT}resets {} (in {})",
                percent(f.pct_x10, view.tenths),
                percent(left, view.tenths),
                clock_local(f.reset, now, tz_at),
                span(f.reset.saturating_sub(now)),
            );
            let _ = write!(
                row,
                "   as of {} ({}, {})",
                span(now.saturating_sub(f.at)),
                source_word(&f.source),
                machine_word(&f.by, look)
            );
            row
        }
        None => format!("{DASH}  {}", missing_text(win, look, tz_at)),
    };
    if let Some(since) = &win.since {
        let _ = write!(row, "{DOT}{}", since_text(since, now, tz_at));
    }
    if win.id == Bucket::Fable.id()
        && let Some(p) = p
    {
        let _ = write!(row, "{DOT}plan: {}", fable_plan_text(p, tz_at));
    }
    format!("  {label:<6} {row}\n")
}

/// The `≈$` row's part for one window, whose spend is `whose`'s: a rolling
/// span another account may have spent in says so (`unattributed`), as a
/// capacity it suppresses does.
fn dollars_part(name: &str, figure: &spend::Figure, whose: spend::Attribution) -> String {
    if figure.rolling {
        let length = figure.to.saturating_sub(figure.from).saturating_sub(1);
        let whose = match whose {
            spend::Attribution::Unattributed { .. } => ", unattributed",
            spend::Attribution::Seen | spend::Attribution::Assumed => "",
        };
        return format!(
            "{name} so far {} (rolling {}{whose})",
            usd(figure.used.usd),
            span(length)
        );
    }
    let mut part = format!("{name} {}", usd(figure.used.usd));
    match &figure.capacity {
        Ok(c) => {
            let _ = write!(
                part,
                " of \u{2265}{} (\u{b1}{:.0}%, uncalibrated)",
                usd(c.usd),
                c.pm_pct
            );
        }
        Err(why) => {
            let _ = write!(part, " (capacity {DASH} {})", why.as_str());
        }
    }
    part
}

/// The limit-reset offer the view reads: the one the accounts state holds of the account the
/// profile names ([`Look::record`]), as last seen, which only [`accounts::note_offer`] records
/// (the resets spec's R22).
fn held_offer<'a>(look: &Look<'a>) -> Option<&'a Offer> {
    look.record().and_then(|a| a.offer.as_ref())
}

/// Whether a held offer's use-by instant has passed: then it is `void`, else `current`. An offer
/// with no use-by read is never `void`: its `as_of` dates it.
fn offer_passed(offer: &Offer, now: i64) -> bool {
    offer.use_by.is_some_and(|by| by <= now)
}

/// Why the `resets` figure is empty, where no offer is held: the state could not be read (every
/// window's word then), the harness's limit reset is off, or it has recorded none.
fn resets_missing(look: &Look<'_>) -> Option<Missing> {
    if held_offer(look).is_some() {
        None
    } else if look.state_unread {
        Some(Missing::StateUnread)
    } else if look.limit_reset {
        Some(Missing::NoGrantRead)
    } else {
        Some(Missing::LimitResetOff)
    }
}

/// The `resets` row: the resets left of the offer as last seen, of their kind where it was
/// named; the use-by instant the confirmation drew, on the local clock at that instant with the
/// time to it, or `passed` from that instant on; and the windows' `as of <age> (<source>,
/// <machine>)`. With no offer, `—  <why>` ([`resets_missing`]).
fn resets_row(look: &Look<'_>, tz_at: &dyn Fn(i64) -> i64) -> String {
    let Some(offer) = held_offer(look) else {
        let why = resets_missing(look).map_or("", Missing::phrase);
        return format!("  {:<6} {DASH}  {why}\n", "resets");
    };
    let mut row = match offer.kind {
        Some(kind) => format!("{} {} left", offer.left, kind.word()),
        None => format!("{} left", offer.left),
    };
    if let Some(by) = offer.use_by {
        let at = clock_local(by, look.now, tz_at);
        if offer_passed(offer, look.now) {
            let _ = write!(row, "{DOT}use by {at}, passed");
        } else {
            let _ = write!(
                row,
                "{DOT}use by {at} (in {})",
                span(by.saturating_sub(look.now))
            );
        }
    }
    let _ = write!(
        row,
        "{DOT}as of {} ({SOURCE_LIMIT_RESET}, {})",
        span(look.now.saturating_sub(offer.at)),
        machine_word(&offer.by, look)
    );
    format!("  {:<6} {row}\n", "resets")
}

/// Render the plain-text view of `look`.
#[must_use]
pub fn text(look: &Look<'_>, view: &View) -> String {
    text_local(look, view, &|_| look.tz_s)
}

/// Render the plain-text view of `look`, placing each wall-clock instant on
/// `tz_at(unix)` so a reset, a login renewal or a Fable end date across a
/// daylight-saving change carries its own UTC offset.
#[must_use]
pub fn text_local(look: &Look<'_>, view: &View, tz_at: &dyn Fn(i64) -> i64) -> String {
    let now = look.now;
    let now_tz = tz_at(now);
    let mut out = format!(
        "accounts at {} {}{DOT}this machine only{DOT}prices {} (list)\n\n",
        day_of(now, now_tz),
        hm(now, now_tz),
        look.prices_as_of.unwrap_or(DASH)
    );
    let p = look.profile.as_ref().ok();
    let plan = match look.profile {
        Ok(p) => p.plan.title().unwrap_or_else(|| format!("plan {DASH}")),
        Err(why) => format!("plan {DASH} ({})", why.word()),
    };
    let credits = p.map_or_else(
        || format!("credits {DASH}"),
        |p| match (&p.credits, p.credits.reason()) {
            (profile::Credits::Off(_), Some(reason)) => {
                format!("credits off ({})", reason.as_str())
            }
            (credits, _) => format!("credits {}", credits.state().unwrap_or(DASH)),
        },
    );
    let _ = write!(out, "{}{DOT}claude{DOT}{plan}{DOT}{credits}", look.label);
    if let Some((money, cache)) = p.and_then(|p| {
        p.usage
            .as_ref()
            .and_then(|u| Some((u.credits.as_ref()?, u)))
    }) && let (Some(spent), Some(cap)) = (money.spent_minor, money.cap_minor)
    {
        #[expect(clippy::cast_precision_loss, reason = "cents of a monthly cap")]
        let (spent, cap) = (spent as f64 / 100.0, cap as f64 / 100.0);
        let _ = write!(
            out,
            ": {} of {} this month (as of {}, /usage)",
            usd(spent),
            usd(cap),
            span(now.saturating_sub(cache.fetched_at_ms.div_euclid(1000)))
        );
    }
    out.push('\n');
    for win in &view.windows {
        out.push_str(&window_row(look, view, win, p, tz_at));
    }
    match look.spend {
        Some(s) => {
            let mut parts = Vec::new();
            for win in &view.windows {
                if win.kind == "model_weekly" && win.current(now).is_none() {
                    continue;
                }
                if let Some(figure) = dollars(look, view, win) {
                    parts.push(dollars_part(
                        &row_label(win),
                        &figure,
                        look.attribution(figure.from),
                    ));
                    if win.id == WEEK_LIMIT {
                        match &figure.left {
                            Ok(left) => parts.push(format!(
                                "\u{2248}{} left (reading {} old)",
                                usd(left.usd),
                                span(left.age_s)
                            )),
                            Err(why) if !figure.rolling => {
                                parts.push(format!("left {DASH} {}", why.as_str()));
                            }
                            Err(_) => {}
                        }
                    }
                }
            }
            if !parts
                .iter()
                .any(|p| p.contains(" of \u{2265}") || p.contains("(capacity"))
            {
                parts.push(format!("capacity {DASH} no reading"));
            }
            let _ = write!(out, "  \u{2248}$     {}", parts.join(DOT));
            if !s.complete {
                out.push_str(" (partial: the fold was cut)");
            }
            out.push('\n');
        }
        None => {
            let _ = writeln!(
                out,
                "  \u{2248}$     {DASH}  no Claude Code directory is known"
            );
        }
    }
    out.push_str(&resets_row(look, tz_at));
    let used = match look.sessions {
        Ok(0) => "here (no session now)".to_owned(),
        Ok(1) => "here (1 session)".to_owned(),
        Ok(n) => format!("here ({n} sessions)"),
        Err(why) => format!("here (sessions not counted: {why})"),
    };
    let _ = write!(out, "  used   {used}");
    if let Some(login) = look.login {
        let _ = write!(out, "{DOT}login: here {}", login.state);
        if let Some(by) = login.renew_by {
            let _ = write!(out, ", renew by {}", clock_local(by, now, tz_at));
        }
    }
    out.push('\n');
    out
}

/// `key` as Unix seconds and `key_rfc3339` as the same instant in UTC, both
/// `null` for an instant nobody read.
fn put_time(o: &mut Map, key: &str, t: Option<i64>) {
    o.insert(key.to_owned(), Value::from(t));
    o.insert(format!("{key}_rfc3339"), Value::from(t.map(rfc3339_utc)));
}

fn word(w: Option<&profile::Word>) -> Value {
    Value::from(w.map(|w| w.as_str().to_owned()))
}

fn finite(v: f64) -> Value {
    if v.is_finite() {
        Value::from(v)
    } else {
        Value::Null
    }
}

fn reason(missing: Missing) -> Value {
    let mut o = Map::new();
    o.insert("reason".to_owned(), Value::from(missing.word()));
    Value::Object(o)
}

/// A window's `since` as `--json` carries it: `{span_s, used_pct, new_window(+), reset_used(+)}`,
/// one of the last three set.
fn since_value(since: &Since) -> Value {
    let mut o = Map::new();
    o.insert("span_s".to_owned(), Value::from(since.span_s));
    let (rose, new_window, reset_used) = match since.moved {
        Moved::Rose(points) => (Some(points), None, None),
        Moved::NewWindow(at) => (None, Some(at), None),
        Moved::ResetUsed(at) => (None, None, Some(at)),
    };
    o.insert("used_pct".to_owned(), Value::from(rose));
    put_time(&mut o, "new_window", new_window);
    put_time(&mut o, "reset_used", reset_used);
    Value::Object(o)
}

/// One window as `--json` carries it.
#[must_use]
pub fn window_value(win: &Win, now: i64) -> Value {
    let mut o = Map::new();
    o.insert("id".to_owned(), Value::from(win.id));
    o.insert("kind".to_owned(), Value::from(win.kind));
    o.insert("name".to_owned(), Value::from(win.name.clone()));
    o.insert("family".to_owned(), Value::Null);
    let current = win.current(now);
    o.insert(
        "used_pct".to_owned(),
        Value::from(current.map(|f| (f.pct_x10 + 5) / 10)),
    );
    o.insert(
        "used_pct_x10".to_owned(),
        Value::from(current.map(|f| f.pct_x10)),
    );
    o.insert(
        "left_pct".to_owned(),
        Value::from(current.map(|f| 100u32.saturating_sub((f.pct_x10 + 5) / 10))),
    );
    let f = win.fig.as_ref();
    put_time(&mut o, "resets_at", win.resets_at());
    put_time(&mut o, "epoch_at", f.and_then(|f| f.epoch_at));
    put_time(&mut o, "void_at", win.void_at);
    o.insert(
        "state".to_owned(),
        Value::from(f.map(|f| if f.current(now) { "current" } else { "void" })),
    );
    put_time(&mut o, "as_of", f.map(|f| f.at));
    o.insert(
        "age_s".to_owned(),
        Value::from(f.map(|f| now.saturating_sub(f.at).max(0))),
    );
    o.insert(
        "source".to_owned(),
        Value::from(f.map(|f| f.source.clone())),
    );
    o.insert("by".to_owned(), Value::from(f.map(|f| f.by.clone())));
    o.insert("cached".to_owned(), Value::from(false));
    o.insert(
        "since".to_owned(),
        win.since.as_ref().map_or(Value::Null, since_value),
    );
    o.insert(
        "missing".to_owned(),
        win.missing.map_or(Value::Null, reason),
    );
    Value::Object(o)
}

fn tokens_json(t: &super::usage::TokenUsage) -> Value {
    let mut o = Map::new();
    o.insert("in".to_owned(), Value::from(t.input));
    o.insert("out".to_owned(), Value::from(t.output));
    o.insert(
        "cw5m".to_owned(),
        Value::from(t.cache_write.saturating_sub(t.cache_write_1h)),
    );
    o.insert("cw1h".to_owned(), Value::from(t.cache_write_1h));
    o.insert("cr".to_owned(), Value::from(t.cache_read));
    Value::Object(o)
}

fn spend_window_json(win: &Win, figure: &spend::Figure, on_credits: bool) -> Value {
    let mut o = Map::new();
    o.insert("id".to_owned(), Value::from(win.id));
    o.insert("rolling".to_owned(), Value::from(figure.rolling));
    put_time(&mut o, "from", Some(figure.from));
    put_time(&mut o, "to", Some(figure.to));
    o.insert("usd".to_owned(), finite(figure.used.usd));
    o.insert("on_credits".to_owned(), Value::from(on_credits));
    o.insert("tokens".to_owned(), tokens_json(&figure.used.tokens));
    o.insert(
        "unpriced".to_owned(),
        Value::Array(
            figure
                .used
                .unpriced
                .iter()
                .map(|(model, t)| {
                    let mut u = Map::new();
                    u.insert("model".to_owned(), Value::from(model.clone()));
                    u.insert("tokens".to_owned(), tokens_json(t));
                    Value::Object(u)
                })
                .collect(),
        ),
    );
    o.insert("approx".to_owned(), Value::from(figure.used.approx));
    let capacity = figure.capacity.as_ref().ok();
    o.insert(
        "capacity_usd".to_owned(),
        capacity.map_or(Value::Null, |c| finite(c.usd)),
    );
    o.insert(
        "capacity_bound".to_owned(),
        Value::from(capacity.map(|_| ">=")),
    );
    let left = figure.left.as_ref().ok();
    o.insert(
        "left_usd".to_owned(),
        left.map_or(Value::Null, |l| finite(l.usd)),
    );
    o.insert("left_age_s".to_owned(), Value::from(left.map(|l| l.age_s)));
    o.insert(
        "pm_pct".to_owned(),
        capacity.map_or(Value::Null, |c| finite(c.pm_pct)),
    );
    o.insert("calibrated".to_owned(), Value::from(false));
    let suppressed = figure
        .capacity
        .as_ref()
        .err()
        .or_else(|| figure.left.as_ref().err())
        .filter(|_| !figure.rolling)
        .map(|w| w.as_str());
    o.insert("suppressed".to_owned(), Value::from(suppressed));
    let mut m = Map::new();
    m.insert("reported".to_owned(), Value::from(1u64));
    m.insert("used".to_owned(), Value::from(1u64));
    o.insert("machines".to_owned(), Value::Object(m));
    Value::Object(o)
}

/// Render the `--json` document of `look` (schema 1).
#[must_use]
pub fn json(look: &Look<'_>, view: &View, aterm: &str) -> String {
    let now = look.now;
    let mut doc = Map::new();
    doc.insert("schema".to_owned(), Value::from(1u64));
    doc.insert("kind".to_owned(), Value::from("account-stats"));
    put_time(&mut doc, "as_of", Some(now));
    let mut missing = Vec::new();
    let mut what = |what: &str, reason: &str| {
        let mut o = Map::new();
        o.insert("what".to_owned(), Value::from(what));
        o.insert("reason".to_owned(), Value::from(reason));
        missing.push(Value::Object(o));
    };
    if let Err(why) = look.profile {
        what("profile", why.word());
    }
    if look.state_unread {
        what("state", Missing::StateUnread.word());
    }
    match look.spend {
        None => what("spend", "no-claude-dir"),
        Some(s) if !s.complete => what("spend", "cut"),
        Some(_) => {}
    }
    if let Err(why) = look.sessions {
        what("used_by", why);
    }
    let complete = look.spend.is_some_and(|s| s.complete);
    doc.insert("complete".to_owned(), Value::from(complete));
    doc.insert("missing".to_owned(), Value::Array(missing));
    let mut machine = Map::new();
    machine.insert(
        "id".to_owned(),
        Value::from(look.machine.map(str::to_owned)),
    );
    machine.insert("name".to_owned(), Value::Null);
    machine.insert("aterm".to_owned(), Value::from(aterm));
    put_time(&mut machine, "clock", Some(now));
    doc.insert("machine".to_owned(), Value::Object(machine));
    doc.insert("fleet".to_owned(), Value::Null);

    let p = look.profile.as_ref().ok();
    let mut a = Map::new();
    a.insert(
        "key".to_owned(),
        Value::from(
            look.key()
                .or_else(|| look.record().and_then(|a| a.key))
                .map(|k| k.to_string()),
        ),
    );
    a.insert("agent".to_owned(), Value::from("claude"));
    a.insert("label".to_owned(), Value::from(look.label));
    a.insert(
        "missing".to_owned(),
        look.profile
            .as_ref()
            .err()
            .map_or(Value::Null, |why| reason(Missing::Profile(*why))),
    );
    a.insert(
        "plan".to_owned(),
        p.map_or(Value::Null, |p| {
            let mut o = Map::new();
            o.insert(
                "word".to_owned(),
                Value::from(p.plan.word.map(|w| w.word())),
            );
            o.insert("tier".to_owned(), word(p.plan.tier.as_ref()));
            o.insert("seat".to_owned(), word(p.plan.seat.as_ref()));
            o.insert("billing".to_owned(), word(p.plan.billing.as_ref()));
            o.insert("display".to_owned(), word(p.plan.display.as_ref()));
            put_time(&mut o, "as_of", p.plan.fetched_at);
            o.insert("source".to_owned(), Value::from("profile"));
            o.insert(
                "by".to_owned(),
                Value::from(look.machine.map(str::to_owned)),
            );
            Value::Object(o)
        }),
    );
    a.insert(
        "windows".to_owned(),
        Value::Array(view.windows.iter().map(|w| window_value(w, now)).collect()),
    );
    a.insert(
        "fable".to_owned(),
        p.map_or(Value::Null, |p| {
            let mut o = Map::new();
            o.insert("plan".to_owned(), Value::from(p.fable.word()));
            put_time(&mut o, "ends_at", p.fable.ends_at());
            o.insert("hint".to_owned(), Value::from(true));
            Value::Object(o)
        }),
    );
    a.insert(
        "credits".to_owned(),
        p.map_or(Value::Null, |p| credits_json(look, p)),
    );
    a.insert("resets".to_owned(), resets_json(look));
    a.insert(
        "spend".to_owned(),
        look.spend.map_or(Value::Null, |s| {
            let mut o = Map::new();
            o.insert("basis".to_owned(), Value::from("list"));
            o.insert(
                "table_as_of".to_owned(),
                Value::from(look.prices_as_of.map(str::to_owned)),
            );
            let figures: Vec<(&Win, spend::Figure)> = view
                .windows
                .iter()
                .filter_map(|w| Some((w, dollars(look, view, w)?)))
                .collect();
            let oldest = figures.iter().map(|(_, f)| f.from).min();
            o.insert(
                "attribution".to_owned(),
                Value::from(look.attribution(oldest.unwrap_or(now)).word()),
            );
            o.insert("complete".to_owned(), Value::from(s.complete));
            let windows = figures
                .iter()
                .map(|(w, figure)| {
                    spend_window_json(w, figure, on_credits(look.record(), figure.from))
                })
                .collect();
            o.insert("windows".to_owned(), Value::Array(windows));
            Value::Object(o)
        }),
    );
    a.insert(
        "login".to_owned(),
        Value::Array(
            look.login
                .into_iter()
                .map(|l| {
                    let mut o = Map::new();
                    o.insert(
                        "machine".to_owned(),
                        Value::from(look.machine.map(str::to_owned)),
                    );
                    o.insert("state".to_owned(), Value::from(l.state));
                    put_time(&mut o, "renew_by", l.renew_by);
                    Value::Object(o)
                })
                .collect(),
        ),
    );
    a.insert(
        "used_by".to_owned(),
        Value::Array(
            look.sessions
                .ok()
                .into_iter()
                .map(|n| {
                    let mut o = Map::new();
                    o.insert(
                        "machine".to_owned(),
                        Value::from(look.machine.map(str::to_owned)),
                    );
                    o.insert("name".to_owned(), Value::from("here"));
                    o.insert(
                        "sessions".to_owned(),
                        Value::from(u64::try_from(n).unwrap_or(u64::MAX)),
                    );
                    put_time(&mut o, "last_seen", Some(now));
                    Value::Object(o)
                })
                .collect(),
        ),
    );
    doc.insert("accounts".to_owned(), Value::Array(vec![Value::Object(a)]));
    aterm_json::to_string(&Value::Object(doc)).unwrap_or_default()
}

/// THE `resets` OBJECT: the limit-reset offer the accounts state holds of the account, as last
/// seen ([`held_offer`]), never one read now. `use_by` is the deadline the confirmation drew
/// (`use by <day> at <time>`, placed in its zone), an instant like every other in the document,
/// and `state` is `void` from it on; `as_of` dates the count. The next-available date is never
/// read, so `next_available_at` is null. With no offer held, every value is null, never 0, and
/// `missing` says why ([`resets_missing`]).
fn resets_json(look: &Look<'_>) -> Value {
    let offer = held_offer(look);
    let mut o = Map::new();
    o.insert("left".to_owned(), Value::from(offer.map(|g| g.left)));
    o.insert(
        "kind".to_owned(),
        Value::from(offer.and_then(|g| g.kind).map(ResetKind::word)),
    );
    put_time(&mut o, "use_by", offer.and_then(|g| g.use_by));
    put_time(&mut o, "next_available_at", None);
    put_time(&mut o, "as_of", offer.map(|g| g.at));
    o.insert(
        "source".to_owned(),
        Value::from(offer.map(|_| SOURCE_LIMIT_RESET)),
    );
    o.insert("by".to_owned(), Value::from(offer.map(|g| g.by.clone())));
    o.insert(
        "state".to_owned(),
        Value::from(offer.map(|g| {
            if offer_passed(g, look.now) {
                "void"
            } else {
                "current"
            }
        })),
    );
    o.insert(
        "missing".to_owned(),
        resets_missing(look).map_or(Value::Null, reason),
    );
    Value::Object(o)
}

/// The `credits` object: the word Claude Code keeps, and the money its
/// cached `/usage` body holds.
fn credits_json(look: &Look<'_>, p: &Profile) -> Value {
    let mut o = Map::new();
    o.insert("state".to_owned(), Value::from(p.credits.state()));
    o.insert("reason".to_owned(), word(p.credits.reason()));
    let cache = p.usage.as_ref();
    let money = cache.and_then(|c| c.credits.as_ref());
    #[expect(clippy::cast_precision_loss, reason = "cents of a monthly cap")]
    let dollars = |minor: Option<i64>| minor.map_or(Value::Null, |m| finite(m as f64 / 100.0));
    o.insert(
        "spent".to_owned(),
        dollars(money.and_then(|m| m.spent_minor)),
    );
    o.insert("cap".to_owned(), dollars(money.and_then(|m| m.cap_minor)));
    o.insert(
        "currency".to_owned(),
        word(money.and_then(|m| m.currency.as_ref())),
    );
    o.insert("balance".to_owned(), Value::Null);
    o.insert("unit".to_owned(), Value::from("usd"));
    o.insert(
        "one_time".to_owned(),
        Value::Array(
            cache
                .into_iter()
                .flat_map(|c| c.one_time.iter())
                .map(|t| {
                    let mut m = Map::new();
                    m.insert("kind".to_owned(), Value::from(t.kind.word()));
                    m.insert("used".to_owned(), t.used_usd.map_or(Value::Null, finite));
                    m.insert("limit".to_owned(), t.limit_usd.map_or(Value::Null, finite));
                    m.insert(
                        "used_pct_x10".to_owned(),
                        Value::from(t.used_pct_x10.map(u32::from)),
                    );
                    put_time(&mut m, "expires_at", t.resets_at);
                    Value::Object(m)
                })
                .collect(),
        ),
    );
    put_time(
        &mut o,
        "as_of",
        cache
            .filter(|_| money.is_some())
            .map(|c| c.fetched_at_ms.div_euclid(1000)),
    );
    o.insert(
        "source".to_owned(),
        Value::from(if money.is_some() {
            "usage-cache"
        } else {
            "profile"
        }),
    );
    o.insert(
        "by".to_owned(),
        Value::from(look.machine.map(str::to_owned)),
    );
    Value::Object(o)
}

#[cfg(test)]
#[path = "account_stats_tests.rs"]
mod tests;
