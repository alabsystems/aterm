// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The accounts outlook as the window holds it (`docs/DESIGN-harness-glass-rainbow-2026-11-05.md`
//! §10): one machine-wide memo on the footer's resolver thread ([`crate::claude_footer`]),
//! which replays the rotation's policy at most once a wall minute (and, while a pane shows a
//! reading the last replay had not seen, at the store's next read), and the [`AccountsSnapshot`]
//! it posts for the Account card, which the main thread maps to rows without reading a file
//! (the card's account rows are the account reader's, read off the GUI thread,
//! `crate::account_menu_read`).
//!
//! The store, the logins and the series are read, and the replay runs on a clone
//! (`aterm_agent::harness::outlook`). The one write is a series' `plan` row, once per tier this
//! process learns from the footer's gated profile read ([`OutlookMemo::note_plan`], §4.2).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use aterm_agent::harness::account_series::{self, SeriesRow};
use aterm_agent::harness::accounts::{self, Mode};
use aterm_agent::harness::outlook::{self, Figure, Outlook, Refusal};
use aterm_agent::harness::quota_view::QuotaView;

/// How often the inputs' stamps are looked at between replays (as the roster's,
/// `claude_footer::ROSTER_RECHECK`).
const LOOK_EVERY: Duration = Duration::from_secs(10);

/// The least time between two replays: the outlook is replayed at most once a minute, but for a
/// replay behind a pane's reading ([`OutlookMemo::quota`]), which waits only for the store's next
/// read.
pub(crate) const REPLAY_EVERY_S: i64 = 60;

/// What the Account card maps to its outlook rows: the outlook the resolver last replayed for
/// Claude Code's pool. The main thread reads no file for it (§10).
#[derive(Debug)]
pub(crate) struct AccountsSnapshot {
    /// The outlook, where one was replayed.
    pub(crate) outlook: Option<Arc<Outlook>>,
}

#[cfg(not(test))]
static SNAPSHOT: std::sync::Mutex<Option<Arc<AccountsSnapshot>>> = std::sync::Mutex::new(None);

#[cfg(test)]
thread_local! {
    /// A test's own snapshot: tests run side by side, each with its own resolver stand-in.
    static SNAPSHOT: std::cell::RefCell<Option<Arc<AccountsSnapshot>>> =
        const { std::cell::RefCell::new(None) };
}

/// The snapshot the resolver last posted, `None` before its first read.
pub(crate) fn snapshot() -> Option<Arc<AccountsSnapshot>> {
    #[cfg(not(test))]
    {
        SNAPSHOT
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
    #[cfg(test)]
    {
        SNAPSHOT.with(|s| s.borrow().clone())
    }
}

fn post(snapshot: Option<Arc<AccountsSnapshot>>) {
    #[cfg(not(test))]
    {
        *SNAPSHOT
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = snapshot;
    }
    #[cfg(test)]
    {
        SNAPSHOT.with(|s| *s.borrow_mut() = snapshot);
    }
    wake_card();
}

/// Wake the open Account card's session, whose footer handler refreshes the card: a new
/// snapshot, or new token facts ([`crate::token_shown::refresh`]).
pub(crate) fn wake_card() {
    if let Some(session) = card_with(|card| card.session) {
        crate::claude_footer::post_changed(session);
    }
}

/// The session whose Account card is open, and whether the next read looks at the store at
/// once ([`card_open`]).
#[derive(Clone, Copy, Debug, Default)]
struct Card {
    session: Option<u64>,
    look: bool,
}

#[cfg(not(test))]
static CARD: std::sync::Mutex<Card> = std::sync::Mutex::new(Card {
    session: None,
    look: false,
});

#[cfg(test)]
thread_local! {
    static CARD: std::cell::Cell<Card> = const { std::cell::Cell::new(Card { session: None, look: false }) };
}

fn card_with<T>(f: impl FnOnce(&mut Card) -> T) -> T {
    #[cfg(not(test))]
    {
        f(&mut CARD
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner))
    }
    #[cfg(test)]
    {
        CARD.with(|c| {
            let mut card = c.get();
            let out = f(&mut card);
            c.set(card);
            out
        })
    }
}

/// The Account card opened for `session` (`Some`), or closed: while it is open, a new
/// snapshot wakes the session's footer handler, which refreshes the card
/// (`App::on_claude_footer_changed`); an open asks the next read to look at the store at once.
pub(crate) fn card_open(session: Option<u64>) {
    card_with(|card| {
        card.look = session.is_some();
        card.session = session;
    });
}

/// What a read of the outlook asks of the window.
#[derive(Clone, Copy)]
pub(crate) struct Reads {
    /// The harness state the accounts store is under.
    pub(crate) state_dir: fn() -> Option<PathBuf>,
    /// `[harness] accounts` and `[harness] usage_credits`, now.
    pub(crate) policy: fn() -> (Mode, bool),
    /// The local offset from UTC at an instant.
    pub(crate) offset: fn(i64) -> i64,
}

/// The window's own: its harness state (in a unit test, the scratch one the test set on its
/// thread, [`TEST_STATE_DIR`]), its switches.
pub(crate) const LIVE: Reads = Reads {
    state_dir: live_state_dir,
    policy: live_policy,
    offset: live_offset,
};

#[cfg(test)]
thread_local! {
    /// The scratch harness state a test's window reads on this thread: none unless it sets one.
    pub(crate) static TEST_STATE_DIR: std::cell::RefCell<Option<PathBuf>> =
        const { std::cell::RefCell::new(None) };
}

fn live_state_dir() -> Option<PathBuf> {
    #[cfg(test)]
    {
        TEST_STATE_DIR.with(|d| d.borrow().clone())
    }
    #[cfg(not(test))]
    {
        crate::harness_host::harness_state()
    }
}

fn live_policy() -> (Mode, bool) {
    (
        crate::harness_host::accounts_mode(),
        crate::harness_host::usage_credits(),
    )
}

fn live_offset(unix: i64) -> i64 {
    crate::autopilot_timers::local_offset_at(unix).unwrap_or(0)
}

/// A file's `(modified, length)`, `None` where it is absent.
type Stamp = Option<(std::time::SystemTime, u64)>;

fn stamp(path: &Path) -> Stamp {
    #[cfg(test)]
    crate::work_counts::outlook_file_look();
    std::fs::metadata(path)
        .ok()
        .map(|m| (m.modified().unwrap_or(std::time::UNIX_EPOCH), m.len()))
}

/// The store as last read.
struct Held {
    roster: accounts::Roster,
    stamps: [Stamp; 2],
    read: Result<(accounts::State, BTreeSet<String>), String>,
    renewals: BTreeMap<String, String>,
}

/// The resolver's memo of the outlook ([`Self::at`]): one for the machine, as the roster's.
#[derive(Default)]
pub(crate) struct OutlookMemo {
    /// What it asks; [`LIVE`] where `None`.
    pub(crate) reads: Option<Reads>,
    /// When the inputs' stamps were last looked at.
    looked: Option<Instant>,
    held: Option<Held>,
    /// Each label's series, read as its file grows ([`account_series::Tail`]).
    series: BTreeMap<String, account_series::Tail>,
    /// The wall second of the last replay, and the policy it ran under.
    replayed: Option<(i64, (Mode, bool))>,
    /// The store was read again since the last replay (its stamps or the roster moved).
    unreplayed: bool,
    outlook: Option<Arc<Outlook>>,
    /// Each label's plan as this process last noted it ([`Self::note_plan`]).
    planned: BTreeMap<String, account_series::PlanSaid>,
    /// The quota views [`Self::quota`] last built, and what it built them of.
    views: Option<(ViewsOf, QuotaViews)>,
    /// Replays run, for the tests that hold it to once a minute.
    #[cfg(test)]
    pub(crate) replays: usize,
    /// Plan rows asked of the series, for the test that holds it to once per tier.
    #[cfg(test)]
    pub(crate) plans_noted: usize,
}

impl OutlookMemo {
    /// The outlook for Claude Code's pool `roster` at wall second `now` (`instant` on the
    /// monotonic clock). The store's and the logins' stamps are looked at most every
    /// [`LOOK_EVERY`] (`force`: now) and the store re-read when they or the roster moved,
    /// or after a failed read; the
    /// replay runs at most once per [`REPLAY_EVERY_S`]. Incompatible or unreadable inputs
    /// withdraw a cached forecast at once without spending or resetting that replay slot.
    /// A move posts a new [`AccountsSnapshot`]. `behind` says a pane shows a reading the last
    /// replay had not seen ([`Self::quota`]): then a store read again since that replay is
    /// replayed at once, not at the minute. The read is at a stamp look, so the replay follows
    /// the harness's fold of the reading within [`LOOK_EVERY`], and a replay that has seen it
    /// ends the pane's `behind`.
    pub(crate) fn at(
        &mut self,
        instant: Instant,
        now: i64,
        roster: Option<&accounts::Roster>,
        force: bool,
        behind: bool,
    ) -> Option<&Outlook> {
        let reads = self.reads.unwrap_or(LIVE);
        let (Some(roster), Some(dir)) = (roster, (reads.state_dir)()) else {
            let held = self.held.take().is_some();
            let outlook = self.outlook.take().is_some();
            if held || outlook {
                post(None);
            }
            self.series.clear();
            return None;
        };
        let roster_changed = self
            .held
            .as_ref()
            .is_some_and(|held| held.roster != *roster);
        let mut moved = roster_changed && self.outlook.take().is_some();
        let force = force || card_with(|card| std::mem::take(&mut card.look));
        let look = force
            || self.held.as_ref().is_none_or(|h| h.roster != *roster)
            || self
                .looked
                .is_none_or(|at| instant.saturating_duration_since(at) >= LOOK_EVERY);
        if look {
            self.looked = Some(instant);
            let store = accounts::Store::at(&dir);
            let stamps = [stamp(&store.state_file()), stamp(&store.logins_file())];
            if self
                .held
                .as_ref()
                .is_none_or(|h| h.roster != *roster || h.stamps != stamps || h.read.is_err())
            {
                self.held = Some(read_held(&store, roster, stamps, now));
                self.unreplayed = true;
                moved = true;
            }
        }
        let policy = (reads.policy)();
        let unavailable =
            roster.rows.is_empty() || self.held.as_ref().is_none_or(|held| held.read.is_err());
        let policy_changed = self.replayed.is_some_and(|(_, used)| used != policy);
        if (unavailable || policy_changed) && self.outlook.take().is_some() {
            moved = true;
        }
        let due = (behind && self.unreplayed)
            || self
                .replayed
                .is_none_or(|(at, _)| now.saturating_sub(at) >= REPLAY_EVERY_S || now < at);
        if due
            && !roster.rows.is_empty()
            && let Some(held) = self.held.as_ref()
            && let Ok((state, enrolled)) = &held.read
        {
            self.replayed = Some((now, policy));
            self.unreplayed = false;
            let series = refresh_series(&mut self.series, &dir, roster, &state.machine, now);
            #[cfg(test)]
            {
                self.replays += 1;
            }
            self.outlook = Some(Arc::new(outlook::outlook(&outlook::Inputs {
                roster,
                state,
                enrolled,
                series: &series,
                renewals: &held.renewals,
                mode: policy.0,
                credits: policy.1,
                now,
                offset: reads.offset,
            })));
            moved = true;
        }
        if moved && self.held.is_some() {
            post(Some(Arc::new(AccountsSnapshot {
                outlook: self.outlook.clone(),
            })));
        }
        self.outlook.as_deref()
    }

    /// The tier the footer's gated profile read named for `label` (its title, `Max 20x`), as a
    /// `plan` row of that label's series (§4.2), asked once per tier this process learns: a
    /// title it already noted asks nothing, and so does a store not read yet. A row the series'
    /// lock kept out waits in the series' outbox for its next append; one whose write failed is
    /// not asked again until the process starts again.
    pub(crate) fn note_plan(&mut self, label: &str, title: &str, now: i64) {
        let Some(said) = outlook::plan_of_title(title) else {
            return;
        };
        if self.planned.get(label) == Some(&said) {
            return;
        }
        let reads = self.reads.unwrap_or(LIVE);
        let Some(dir) = (reads.state_dir)() else {
            return;
        };
        let Some(Ok((state, _))) = self.held.as_ref().map(|h| &h.read) else {
            return;
        };
        let machine = state.machine.clone();
        #[cfg(test)]
        {
            self.plans_noted += 1;
        }
        let _ = account_series::note_plan(&dir, label, &machine, &said, now);
        self.planned.insert(label.to_owned(), said);
    }

    /// The quota views of the pane whose account is `tab` ([`Outlook::quota`]): as replayed,
    /// and with observations only ([`Outlook::observations_only`]) for a pane whose progress,
    /// read after the resolver, refuses its forecasts (`FooterFacts::quota_view`); the second
    /// `None` where it says what the first does. `own` is the footer's own figure of the week,
    /// `movable` whether the harness can move its tab. Built once per replay and input, so a
    /// resolver pass between replays builds none. Where `own` holds a figure of a row the last
    /// replay read nothing of, that reading came since the replay: the next store read, which
    /// carries the harness's fold of it, replays at once ([`Self::at`]), so the step,
    /// the curve and the tail follow the lead within [`LOOK_EVERY`] of the fold, not the minute.
    pub(crate) fn quota(
        &mut self,
        (instant, now): (Instant, i64),
        roster: Option<&accounts::Roster>,
        tab: &str,
        own: Option<Figure>,
        movable: bool,
    ) -> Option<QuotaViews> {
        let behind = own.is_some_and(|f| f != Figure::Unread)
            && self.outlook.as_ref().is_some_and(|o| {
                o.rows
                    .iter()
                    .any(|r| r.label == tab && r.figure(o.as_of) == Figure::Unread)
            });
        self.at(instant, now, roster, false, behind)?;
        let outlook = self.outlook.clone()?;
        if let Some((of, views)) = &self.views
            && Arc::ptr_eq(&of.outlook, &outlook)
            && of.tab == tab
            && of.own == own
            && of.movable == movable
        {
            return Some(views.clone());
        }
        let view = outlook.quota(tab, own, movable);
        // The observed view's words never name the refusal (it has no carrier, so no tail
        // says it): either refusal builds the same view.
        let observed = outlook
            .observations_only(Refusal::SessionPaused)
            .quota(tab, own, movable);
        let views = QuotaViews {
            observed: (observed != view).then_some(observed),
            view,
        };
        self.views = Some((
            ViewsOf {
                outlook,
                tab: tab.to_owned(),
                own,
                movable,
            },
            views.clone(),
        ));
        Some(views)
    }
}

/// A pane's quota views ([`OutlookMemo::quota`]).
#[derive(Clone, Debug)]
pub(crate) struct QuotaViews {
    /// As replayed.
    pub(crate) view: QuotaView,
    /// With observations only, where that differs.
    pub(crate) observed: Option<QuotaView>,
}

/// What [`OutlookMemo::quota`]'s views were built of: the replay (by identity) and the inputs.
struct ViewsOf {
    outlook: Arc<Outlook>,
    tab: String,
    own: Option<Figure>,
    movable: bool,
}

/// The store's state, enrolment and renewals for `roster` (read only).
fn read_held(
    store: &accounts::Store,
    roster: &accounts::Roster,
    stamps: [Stamp; 2],
    now: i64,
) -> Held {
    #[cfg(test)]
    {
        crate::work_counts::account_store_read();
        crate::work_counts::outlook_file_look();
    }
    let read = store
        .load(now)
        .map(|(state, _)| (state, accounts::enrolled(roster)));
    let renewals = store
        .load_logins(now)
        .map(|(logins, _)| outlook::renewals_of(roster, &logins))
        .unwrap_or_default();
    Held {
        roster: roster.clone(),
        stamps,
        read,
        renewals,
    }
}

/// Each roster label's series rows as the outlook reads them at `now`: a `stat` per file, and
/// only the bytes appended since the last read (the whole file again after a compaction's
/// rename, [`account_series::Tail`]).
fn refresh_series(
    cache: &mut BTreeMap<String, account_series::Tail>,
    dir: &Path,
    roster: &accounts::Roster,
    machine: &str,
    now: i64,
) -> BTreeMap<String, Vec<SeriesRow>> {
    cache.retain(|label, _| roster.row(label).is_some());
    let mut out = BTreeMap::new();
    for row in &roster.rows {
        let Some(path) = account_series::path_of(dir, &row.label, machine) else {
            cache.remove(&row.label);
            continue;
        };
        let tail = cache
            .entry(row.label.clone())
            .and_modify(|tail| {
                if tail.path() != path {
                    *tail = account_series::Tail::new(path.clone());
                }
            })
            .or_insert_with(|| account_series::Tail::new(path.clone()));
        #[cfg(test)]
        crate::work_counts::outlook_file_look();
        tail.refresh(now);
        if !tail.rows().is_empty() {
            out.insert(row.label.clone(), tail.rows().to_vec());
        }
    }
    out
}

#[cfg(test)]
#[path = "accounts_outlook_tests.rs"]
mod tests;
