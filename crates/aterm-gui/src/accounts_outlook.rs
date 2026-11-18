// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The accounts outlook as the window holds it (`docs/DESIGN-harness-glass-rainbow-2026-11-05.md`
//! §10): one machine-wide memo on the footer's resolver thread ([`crate::claude_footer`]),
//! which replays the rotation's policy only for a cause (the footer cause design's §2.5: a moved
//! input, the journal or the policy moving, a sign-in starting or ending, a time the last replay
//! computed coming due, the Account card opening), and the [`AccountsSnapshot`] it posts for the
//! Account card, which the main thread maps to rows without reading a file (the card's account
//! rows are the account reader's, read off the GUI thread, `crate::account_menu_read`).
//!
//! The store, the logins and the series are read, and the replay runs on a clone
//! (`aterm_agent::harness::outlook`). The one write is a series' `plan` row, once per tier this
//! process learns from the footer's gated profile read ([`OutlookMemo::note_plan`], §4.2).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use aterm_agent::harness::account_series::{self, SeriesRow};
use aterm_agent::harness::accounts::{self, Mode};
use aterm_agent::harness::alab_seen;
use aterm_agent::harness::looks::{self, Heed, Input};
use aterm_agent::harness::outlook::{self, Figure, Outlook, Refusal};
use aterm_agent::harness::quota_view::QuotaView;

/// The least wall time between two replays a moved input owes ([`OutlookMemo::at`]): a store the
/// harness writes at each fold it makes is replayed at most once per this while the writes go on,
/// but for a replay behind a pane's reading ([`OutlookMemo::quota`]), which runs at once. A replay
/// another cause owes (a deadline, the card, the journal, the policy, a sign-in) runs at once.
pub(crate) const REPLAY_FLOOR_S: i64 = 60;

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
/// snapshot, or new token facts ([`crate::token_shown::post`]).
pub(crate) fn wake_card() {
    if let Some(session) = card_with(|card| card.session) {
        crate::claude_footer::post_changed(session);
    }
}

/// The session whose Account card is open, and whether the next read replays the outlook at
/// once ([`card_open`]).
#[derive(Clone, Copy, Debug, Default)]
struct Card {
    session: Option<u64>,
    demand: bool,
}

#[cfg(not(test))]
static CARD: std::sync::Mutex<Card> = std::sync::Mutex::new(Card {
    session: None,
    demand: false,
});

#[cfg(test)]
thread_local! {
    static CARD: std::cell::Cell<Card> = const { std::cell::Cell::new(Card { session: None, demand: false }) };
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
/// (`App::on_claude_footer_changed`); an open asks the next read to replay the outlook at once,
/// which the card's own ask of the session's footer makes (`App::request_account_read`).
pub(crate) fn card_open(session: Option<u64>) {
    card_with(|card| {
        card.demand = session.is_some();
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
    /// The lab's journal as this process holds it, never waited for ([`alab_seen::journal`]).
    pub(crate) journal: fn() -> alab_seen::Journal,
}

/// The window's own: its harness state (in a unit test, the scratch one the test set on its
/// thread, [`TEST_STATE_DIR`]), its switches.
pub(crate) const LIVE: Reads = Reads {
    state_dir: live_state_dir,
    policy: live_policy,
    offset: live_offset,
    journal: live_journal,
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
    #[cfg(test)]
    crate::host_facts::noted("local_offset");
    crate::autopilot_timers::local_offset_at(unix).unwrap_or(0)
}

fn live_journal() -> alab_seen::Journal {
    #[cfg(test)]
    crate::host_facts::noted("journal");
    alab_seen::journal()
}

/// The store as last read for a roster: its state's and its bindings' loads as
/// [`OutlookMemo::stores`] held them, by their files' stamps.
struct Held {
    roster: accounts::Roster,
    stamps: Vec<crate::footer_inputs::Stamp>,
    read: Result<(Arc<accounts::State>, BTreeSet<String>), String>,
    renewals: BTreeMap<String, String>,
}

/// The resolver's memo of the outlook ([`Self::at`]): one for the machine, as the roster's.
#[derive(Default)]
pub(crate) struct OutlookMemo {
    /// What it asks; [`LIVE`] where `None`.
    pub(crate) reads: Option<Reads>,
    held: Option<Held>,
    /// The accounts stores as the resolver last loaded them, by their harness state: this
    /// outlook's, the footer's own week (`claude_footer::own_stored_week`) and the roster's
    /// bindings (`claude_footer::RosterMemo::at`) share each load.
    pub(crate) stores: crate::footer_inputs::Stores,
    /// Each label's series, read as its file grows ([`account_series::Tail`]).
    series: BTreeMap<String, account_series::Tail>,
    /// Each label's series file's stamp as the last look found it ([`Input::Series`]).
    series_stamps: BTreeMap<String, crate::footer_inputs::Stamp>,
    /// The wall second of the last replay, and the policy it ran under.
    replayed: Option<(i64, (Mode, bool))>,
    /// An input moved since the last replay: the store was read again (its stamps or the roster
    /// moved), or a series' file moved.
    unreplayed: bool,
    /// The record of the sign-in this Mac runs ([`accounts::Store::signing_in_record`]), held by
    /// its file's stamp ([`Input::SignIn`]): read again only where the stamp moved.
    sign_in: Option<(crate::footer_inputs::Stamp, Option<accounts::RunningSignIn>)>,
    /// The row whose sign-in runs on this Mac now, as the last look read it: the held record's,
    /// while its process lives ([`accounts::RunningSignIn::lives`]).
    signing_in: Option<String>,
    /// [`Self::signing_in`] moved since the last replay: the next one runs at once, so a row
    /// says `signing in`, and stops saying it, at the read the record's watch owes.
    sign_in_moved: bool,
    /// The local time zone was set since the last replay ([`Self::zone_set`]): the next one runs
    /// at once, so the outlook's local days and times are the new zone's.
    zoned: bool,
    /// The journal the last replay read, or one that says what it says ([`alab_seen::journal`]).
    journal: Option<alab_seen::Journal>,
    outlook: Option<Arc<Outlook>>,
    /// The earliest wall second at which a replay of the inputs the last one read says
    /// otherwise ([`Outlook::changes_after`]): at it the next replay runs, whatever the minute.
    deadline: Option<i64>,
    /// Each label's plan as this process last noted it ([`Self::note_plan`]).
    planned: BTreeMap<String, account_series::PlanSaid>,
    /// The quota views [`Self::quota`] last built, and what it built them of.
    views: Option<(ViewsOf, QuotaViews)>,
    /// Replays run, for the tests that hold them to their causes.
    #[cfg(test)]
    pub(crate) replays: usize,
    /// Plan rows asked of the series, for the test that holds it to once per tier.
    #[cfg(test)]
    pub(crate) plans_noted: usize,
}

impl OutlookMemo {
    /// The outlook for Claude Code's pool `roster` at wall second `now`. Each call looks at the
    /// stamps of the files the store's and the logins' loads read, the sign-in record's and each
    /// roster row's series' ([`looks::note`], so the resolver watches each); the store is loaded
    /// again only where one moved ([`crate::footer_inputs::Stores`], a failed load included), and
    /// held again for a moved roster. It replays only for a cause: the first time; the Account
    /// card opening ([`card_open`]); the policy, the journal or the running sign-in moving; a
    /// new local time zone ([`Self::zone_set`]); the deadline the last replay computed
    /// ([`Outlook::changes_after`]) passing; and an input that
    /// moved since the last replay (the store, the roster, a series), once [`REPLAY_FLOOR_S`] has
    /// passed since that replay, or at once where `behind` says a pane shows a reading the last
    /// replay had not seen ([`Self::quota`]), so a replay that has seen it ends the pane's
    /// `behind`. Incompatible or unreadable inputs withdraw a cached forecast at once without
    /// spending or resetting the replay's floor. A move posts a new [`AccountsSnapshot`].
    pub(crate) fn at(
        &mut self,
        now: i64,
        roster: Option<&accounts::Roster>,
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
            self.series_stamps.clear();
            return None;
        };
        let roster_changed = self
            .held
            .as_ref()
            .is_some_and(|held| held.roster != *roster);
        let mut moved = roster_changed && self.outlook.take().is_some();
        let demand = card_with(|card| std::mem::take(&mut card.demand));
        #[cfg(test)]
        crate::work_counts::outlook_file_look();
        let state = self.stores.state(&dir, now);
        let (mut stamps, state) = (state.stamps().to_vec(), state.value().clone());
        let logins = self.stores.logins(&dir, now);
        stamps.extend_from_slice(logins.stamps());
        if self
            .held
            .as_ref()
            .is_none_or(|h| h.roster != *roster || h.stamps != stamps)
        {
            self.held = Some(held_of(roster, stamps, state, logins.value()));
            self.unreplayed = true;
            moved = true;
        }
        let signing_in = self.sign_in_at(&dir);
        if self.signing_in != signing_in {
            self.signing_in = signing_in;
            self.sign_in_moved = true;
        }
        if self.look_at_series(&dir, roster) {
            self.unreplayed = true;
        }
        let policy = (reads.policy)();
        let unavailable =
            roster.rows.is_empty() || self.held.as_ref().is_none_or(|held| held.read.is_err());
        let policy_changed = self.replayed.is_some_and(|(_, used)| used != policy);
        if (unavailable || policy_changed) && self.outlook.take().is_some() {
            moved = true;
        }
        let journal = (reads.journal)();
        let journal_moved = match &self.journal {
            Some(seen) if says_the_same(seen, &journal) => {
                self.journal = Some(journal.clone());
                false
            }
            Some(_) => true,
            None => false,
        };
        let floor = self
            .replayed
            .is_none_or(|(at, _)| now.saturating_sub(at) >= REPLAY_FLOOR_S || now < at);
        let due = self.replayed.is_none()
            || demand
            || policy_changed
            || journal_moved
            || self.sign_in_moved
            || self.zoned
            || self.deadline.is_some_and(|deadline| now >= deadline)
            || (self.unreplayed && (behind || floor));
        if due
            && !roster.rows.is_empty()
            && let Some(held) = self.held.as_ref()
            && let Ok((state, enrolled)) = &held.read
        {
            self.replayed = Some((now, policy));
            self.unreplayed = false;
            self.sign_in_moved = false;
            self.zoned = false;
            let series = refresh_series(&mut self.series, &dir, roster, &state.machine, now);
            #[cfg(test)]
            {
                self.replays += 1;
                crate::work_counts::outlook_replayed();
            }
            // The journal as this process holds it, never waited for: a peer's bound of a row
            // this Mac read nothing current of, for its line and the lean's best case alone.
            let lab = match &journal {
                alab_seen::Journal::Read(seen) => {
                    alab_seen::bounds(accounts::Cli::Claude, roster, seen, now)
                }
                alab_seen::Journal::Off | alab_seen::Journal::NotReadYet => BTreeMap::new(),
            };
            self.journal = Some(journal);
            let inputs = outlook::Inputs {
                roster,
                state,
                enrolled,
                series: &series,
                renewals: &held.renewals,
                lab: &lab,
                mode: policy.0,
                credits: policy.1,
                now,
                offset: reads.offset,
                signing_in: self.signing_in.as_deref(),
            };
            let replayed = outlook::outlook(&inputs);
            self.deadline = replayed.changes_after(&inputs);
            self.outlook = Some(Arc::new(replayed));
            moved = true;
        }
        if moved && self.held.is_some() {
            post(Some(Arc::new(AccountsSnapshot {
                outlook: self.outlook.clone(),
            })));
        }
        self.outlook.as_deref()
    }

    /// The local time zone was set ([`crate::host_facts::Bell::Zone`]): the outlook's local
    /// days and times, its midnights and its `↺ Sat 10:00`, were reckoned in the old zone's
    /// offsets ([`Reads::offset`]), so the next [`Self::at`] replays it whatever the floor.
    pub(crate) fn zone_set(&mut self) {
        self.zoned = true;
    }

    /// The sign-in this Mac runs, as the last look read it running ([`Self::signing_in`]): its
    /// record, whose holder the resolver watches for the exit that ends it, its record left
    /// standing where it was killed.
    pub(crate) fn sign_in_running(&self) -> Option<&accounts::RunningSignIn> {
        self.signing_in.as_ref()?;
        self.sign_in.as_ref()?.1.as_ref()
    }

    /// The row whose sign-in runs on this Mac now ([`accounts::Store::signing_in`]): the record
    /// its file holds, read again only where the file's stamp moved, while the process it names
    /// lives, as the process table says at each call ([`accounts::RunningSignIn::lives`]).
    fn sign_in_at(&mut self, dir: &Path) -> Option<String> {
        let store = accounts::Store::at(dir);
        let stamp = looks::note(Input::SignIn, &store.signing_in_file(), Heed::All);
        if self.sign_in.as_ref().is_none_or(|(held, _)| *held != stamp) {
            let record = stamp.and_then(|_| store.signing_in_record());
            self.sign_in = Some((stamp, record));
        }
        self.sign_in
            .as_ref()
            .and_then(|(_, record)| record.as_ref())
            .filter(|record| record.lives())
            .map(|record| record.label().to_owned())
    }

    /// Look at each roster row's series file on this machine ([`Input::Series`]): whether one's
    /// stamp moved since the last look. The series are read only at a replay
    /// ([`refresh_series`]); with no store held yet there is no machine to name them by.
    fn look_at_series(&mut self, dir: &Path, roster: &accounts::Roster) -> bool {
        let Some(Ok((state, _))) = self.held.as_ref().map(|held| &held.read) else {
            return false;
        };
        let stamps: BTreeMap<String, crate::footer_inputs::Stamp> = roster
            .rows
            .iter()
            .filter_map(|row| {
                let path = account_series::path_of(dir, &row.label, &state.machine)?;
                Some((
                    row.label.clone(),
                    looks::note(Input::Series, &path, Heed::Contents),
                ))
            })
            .collect();
        let moved = stamps != self.series_stamps;
        self.series_stamps = stamps;
        moved
    }

    /// The wall second from which a read replays with no other cause, past or not: the earliest
    /// of [`Self::owed_terms`]. A read at it or later replays, and a read before it does not
    /// unless another cause moved ([`Self::at`]).
    #[cfg(all(test, unix))]
    pub(crate) fn replay_owed_at(&self) -> Option<i64> {
        self.owed_terms().min()
    }

    /// The earliest wall second after `now` at which a replay is owed with no other cause, of
    /// [`Self::owed_terms`]. `None` where none is ahead (a deadline past runs its replay at the
    /// next read).
    pub(crate) fn changes_after(&self, now: i64) -> Option<i64> {
        self.owed_terms().filter(|t| *t > now).min()
    }

    /// The times a replay is owed with no other cause: the deadline the last replay computed
    /// ([`Self::deadline`]), while its outlook is held, and the end of [`REPLAY_FLOOR_S`] after
    /// it while an input that moved since waits for it.
    fn owed_terms(&self) -> impl Iterator<Item = i64> {
        let deadline = self.outlook.as_ref().and(self.deadline);
        let owed = self
            .held
            .as_ref()
            .filter(|held| self.unreplayed && held.read.is_ok())
            .and(self.replayed)
            .map(|(at, _)| at + REPLAY_FLOOR_S);
        deadline.into_iter().chain(owed)
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
    /// replay read nothing of, that reading came since the replay: the store read again since,
    /// which carries the harness's fold of it, replays at once ([`Self::at`]), so the step, the
    /// curve and the tail follow the lead at the read the store's watch owes, not the floor.
    pub(crate) fn quota(
        &mut self,
        now: i64,
        roster: Option<&accounts::Roster>,
        tab: &str,
        own: Option<Figure>,
        movable: bool,
    ) -> Option<QuotaViews> {
        let behind = own.is_some_and(|f| f != Figure::Unread)
            && self.outlook.as_ref().is_some_and(|o| {
                o.rows
                    .iter()
                    .any(|r| r.label == tab && r.week_figure(o.as_of) == Figure::Unread)
            });
        self.at(now, roster, behind)?;
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

/// Whether journal `held` says what `now` does: the same read, or one that says the same at
/// another instant ([`alab_seen::Seen::says_the_same`]: the replay reads no read's instant).
fn says_the_same(held: &alab_seen::Journal, now: &alab_seen::Journal) -> bool {
    match (held, now) {
        (alab_seen::Journal::Read(held), alab_seen::Journal::Read(now)) => {
            Arc::ptr_eq(held, now) || held.says_the_same(now)
        }
        (held, now) => held == now,
    }
}

/// The store's state, enrolment and renewals for `roster`, of the loads `state` and `logins`
/// held at `stamps`.
fn held_of(
    roster: &accounts::Roster,
    stamps: Vec<crate::footer_inputs::Stamp>,
    state: crate::footer_inputs::Loaded<accounts::State>,
    logins: &crate::footer_inputs::Loaded<accounts::Logins>,
) -> Held {
    let renewals = logins
        .as_ref()
        .map(|logins| outlook::renewals_of(roster, logins))
        .unwrap_or_default();
    Held {
        roster: roster.clone(),
        stamps,
        read: state.map(|state| (state, accounts::enrolled(roster))),
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
