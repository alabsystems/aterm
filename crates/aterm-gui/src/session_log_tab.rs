// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE TAB MARKS (the Workers as subagents design (2026-11-04) §7, the close's totals §8):
//! each act on a session's tab is a `tab` mark in that session's log, attributed to who acted.
//!
//! ```text
//! mark - tab act=open why=<gesture|act> parent=<sid> window=<n> parked=0|1 by=<actor> src=aterm conf=exact
//! mark - tab act=keep parent=<sid> window=<n> parked=0 by=<actor> src=aterm conf=exact
//! mark - tab act=close why=<act> parent=<sid> window=<n> parked=0|1 out=<exit reason> task=<state>[:<why>]
//!     wall_s=<s> cpu_s=<s> peak_mb=<MB> tokens=<n> account=<login> by=<actor> src=aterm conf=exact
//! ```
//!
//! The open records where the tab took its place ([`TabPlace`]), which the log keeps and a
//! seamless update carries (`tab v=1`, the carry module), so the close names the same window and
//! parent and measures `wall_s=` from the open; the App keeps the place's window and whether it
//! is parked as the tab moves ([`SessionLog::place_tab`]). The close's other totals are what the
//! log was told the tab spent ([`TabSpend`]): its agents' tokens and login as the footer read them
//! ([`SessionLog::note_agent_usage`]), and its process tree's CPU and peak memory as one read of
//! it found them before its terminal closed ([`SessionLog::note_tree_spent`]). A total aterm could
//! not read is written `unavailable` ([`aterm_slog::UNAVAILABLE`]), never `0`.

use super::SessionLog;
use aterm_slog::{Actor, Conf, Fields, Key, Kind, Mark, Node, ShortId, Src, Stamp, TabAct};

/// Where a session's tab took its place, as its `act=open` said.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TabPlace {
    /// Wall ms of the open: the close's `wall_s=` counts from it.
    pub(crate) opened_wall: u64,
    /// The window's number (`WindowId`).
    pub(crate) window: u64,
    /// Whether it sits under its window's Workers tab, out of the strip.
    pub(crate) parked: bool,
    /// The session the tab was opened from (its lineage parent), if any.
    pub(crate) parent: Option<ShortId>,
}

/// What a tab's process tree spent over its life, as one read of it found it
/// (`crate::tab_marks::tree_spent`), each `None` where nothing in it could be read.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct TreeSpent {
    /// CPU nanoseconds: every live process's own and what each reaped of its children.
    pub(crate) cpu_ns: Option<u64>,
    /// The largest footprint, bytes, any process alive at the read ever reached.
    pub(crate) peak_bytes: Option<u64>,
}

/// What the log was told a tab spent, for its close's totals (§8).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TabSpend {
    /// The tokens of the agent runs before [`Self::run`]: `None` once one of them ended unread.
    banked: Option<u64>,
    /// The agent run the footer reads now: its conversation (`c:<id>`), else its process
    /// (`p:<pgid>:<start>`); `None` before any.
    run: Option<String>,
    /// That run's tokens (input plus output) as its last whole read counted them.
    run_tokens: Option<u64>,
    /// The roster's label for the login an agent of the tab last ran under.
    account: Option<String>,
    /// The process tree's spend, once read.
    tree: Option<TreeSpent>,
}

impl Default for TabSpend {
    fn default() -> Self {
        Self {
            banked: Some(0),
            run: None,
            run_tokens: None,
            account: None,
            tree: None,
        }
    }
}

impl TabSpend {
    /// Every agent run's tokens, or `None` where any run's were never read whole (or none ran).
    fn tokens(&self) -> Option<u64> {
        self.run.as_ref()?;
        self.banked
            .zip(self.run_tokens)
            .map(|(banked, run)| banked.saturating_add(run))
    }
}

/// A tab's agent spend as a seamless update carries it (`spend v=1`, [`SessionLog::spend_carry`]).
#[cfg(any(unix, test))]
pub(super) struct SpendCarry<'a> {
    /// What the agent runs before [`Self::run`] spent: `None` once one of them ended unread.
    pub(super) banked: Option<u64>,
    /// The run the footer reads now.
    pub(super) run: &'a str,
    /// That run's tokens as its last whole read counted them.
    pub(super) run_tokens: Option<u64>,
    /// The roster's label for the login.
    pub(super) account: Option<&'a str>,
}

/// What a close's totals read (§8), each `None` where aterm could not read it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct TabTotals {
    /// CPU-seconds the tab's processes used over its life.
    cpu_s: Option<u64>,
    /// The tab's peak memory footprint, MiB: the largest any of its processes alive at its
    /// close reached.
    peak_mb: Option<u64>,
    /// Tokens its agents spent (input plus output), from usage reads that saw each run whole.
    tokens: Option<u64>,
    /// The roster's label for the login its agent ran under.
    account: Option<String>,
}

/// One `act=close` as the registry lets a placed session go.
#[derive(Clone, Debug)]
pub(crate) struct TabClose<'a> {
    /// The exit reason's word, the session close's `out=`.
    pub(crate) out: aterm_slog::SessionOut,
    /// Who closed it.
    pub(crate) by: Actor,
    /// The act of aterm's own that closed it (`concluded`, `unstarted`), if one did.
    pub(crate) why: Option<&'static str>,
    /// A worker's task as it stood: `<state>` or `<state>:<why>`.
    pub(crate) task: Option<&'a str>,
}

impl TabPlace {
    fn fields(&self, act: TabAct, why: Option<&'static str>) -> Fields {
        let f = Fields::new().word(Key::Act, act.word()).unwrap_or_default();
        let f = match why {
            Some(w) => f.clone().word(Key::Why, w).unwrap_or(f),
            None => f,
        };
        let f = match self.parent {
            Some(p) => f.clone().id(Key::Parent, p.as_str()).unwrap_or(f),
            None => f,
        };
        let f = f.clone().u(Key::Window, self.window).unwrap_or(f);
        f.clone()
            .u(Key::Parked, u64::from(self.parked))
            .unwrap_or(f)
    }
}

impl SessionLog {
    /// `mark - tab act=open`: the session took `place` (its open's wall time the place's), opened
    /// `why` by `by`.
    pub(crate) fn open_tab(&mut self, place: TabPlace, why: &'static str, by: Actor, at: Stamp) {
        self.tab = Some(place);
        let f = place.fields(TabAct::Open, Some(why));
        self.tab_mark(f, by, at);
    }

    /// `mark - tab act=keep`: a parked worker's tab became `by`'s own; the place it keeps is no
    /// longer parked. Nothing for a session no open placed.
    pub(crate) fn keep_tab(&mut self, by: Actor, at: Stamp) {
        let Some(place) = self.tab.as_mut() else {
            return;
        };
        place.parked = false;
        let f = place.fields(TabAct::Keep, None);
        self.tab_mark(f, by, at);
    }

    /// `mark - tab act=close` with the totals (§8): written before the session's own close, and
    /// only for a session an open placed (a session no tab held, a test's registry entry or a
    /// headless one, has no tab to close).
    pub(crate) fn close_tab(&mut self, close: TabClose<'_>, at: Stamp) {
        let Some(place) = self.tab.take() else {
            return;
        };
        let mut f = place.fields(TabAct::Close, close.why);
        let _ = f.set(
            Key::Out,
            aterm_slog::Val::Word(aterm_slog::Out::Session(close.out).word()),
        );
        if let Some(task) = close.task
            && let Some(tok) = aterm_slog::BoundedToken::new(Key::TaskEnd, task)
        {
            let _ = f.set(Key::TaskEnd, aterm_slog::Val::Tok(tok));
        }
        let wall_s = at
            .wall_ms
            .checked_sub(place.opened_wall)
            .map(|ms| ms / 1000);
        let spend = std::mem::take(&mut self.spend);
        let tree = spend.tree.unwrap_or_default();
        let totals = TabTotals {
            cpu_s: tree.cpu_ns.map(|ns| ns / 1_000_000_000),
            peak_mb: tree.peak_bytes.map(|bytes| bytes / (1024 * 1024)),
            tokens: spend.tokens(),
            account: spend.account,
        };
        let f = [
            (Key::WallS, wall_s),
            (Key::CpuS, totals.cpu_s),
            (Key::PeakMb, totals.peak_mb),
            (Key::Tokens, totals.tokens),
        ]
        .into_iter()
        .fold(f, |f, (key, sample)| {
            f.clone().sample(key, sample).unwrap_or(f)
        });
        let account = totals.account.as_deref().unwrap_or(aterm_slog::UNAVAILABLE);
        let f = f.clone().text(Key::Account, account).unwrap_or(f);
        self.tab_mark(f, close.by, at);
    }

    /// The placed tab now stands in `window`, parked under its Workers tab or not: a move to
    /// another window, a restore that parked it, a Keep as Tab. No mark (a move is no act); only
    /// the close's place follows. Nothing for a session no open placed.
    pub(crate) fn place_tab(&mut self, window: u64, parked: bool) {
        if let Some(place) = self.tab.as_mut() {
            place.window = window;
            place.parked = parked;
        }
    }

    /// Whether an open placed the session's tab (and no close took it since).
    pub(crate) const fn tab_placed(&self) -> bool {
        self.tab.is_some()
    }

    /// The footer read agent run `run` (its conversation, else its process): `tokens` its input
    /// and output as a whole read of it counted them (`None` from a partial read, which keeps the
    /// last whole one), `account` the roster's label for its login. A new run banks the last
    /// one's tokens, or, where that one was never read whole, leaves the tab's tokens unread.
    pub(crate) fn note_agent_usage(
        &mut self,
        run: String,
        tokens: Option<u64>,
        account: Option<String>,
    ) {
        let spend = &mut self.spend;
        if spend.run.as_deref() != Some(run.as_str()) {
            if spend.run.is_some() {
                spend.banked = spend
                    .banked
                    .zip(spend.run_tokens)
                    .map(|(banked, last)| banked.saturating_add(last));
            }
            spend.run = Some(run);
            spend.run_tokens = None;
        }
        if tokens.is_some() {
            spend.run_tokens = tokens;
        }
        if account.is_some() {
            spend.account = account;
        }
    }

    /// One read of the tab's process tree, taken as its terminal closes ([`TreeSpent`]).
    pub(crate) const fn note_tree_spent(&mut self, spent: TreeSpent) {
        self.spend.tree = Some(spent);
    }

    /// Where the session's tab stands, as its open (and a keep, a move) said.
    #[cfg(test)]
    pub(crate) const fn tab_place(&self) -> Option<TabPlace> {
        self.tab
    }

    /// Put back a carried place (`tab v=1`).
    pub(super) fn carry_tab(&mut self, place: TabPlace) {
        self.tab = Some(place);
    }

    /// The agent spend a carry line names, as [`Self::spend_carry`] wrote it.
    pub(super) fn carry_spend(
        &mut self,
        banked: Option<u64>,
        run: Option<String>,
        run_tokens: Option<u64>,
        account: Option<String>,
    ) {
        self.spend = TabSpend {
            banked,
            run,
            run_tokens,
            account,
            tree: None,
        };
    }

    /// The agent spend to carry (`spend v=1`), once an agent run was read.
    #[cfg(any(unix, test))]
    pub(super) fn spend_carry(&self) -> Option<SpendCarry<'_>> {
        let spend = &self.spend;
        Some(SpendCarry {
            banked: spend.banked,
            run: spend.run.as_deref()?,
            run_tokens: spend.run_tokens,
            account: spend.account.as_deref(),
        })
    }

    fn tab_mark(&mut self, f: Fields, by: Actor, at: Stamp) {
        let _ = self.log.mark(
            Mark::new(Kind::Tab, Src::Aterm, Conf::Exact)
                .at(Node::SESSION)
                .by(by)
                .fields(f),
            at,
        );
    }
}
