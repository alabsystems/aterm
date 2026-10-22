// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The presence driver: the sequencing every host shares. It folds a
//! session's facts into its slot, projects every window showing it — the
//! window's MOST PRESSING session ([`Desk::pressing_session`]) lights its
//! border and pill, whichever tab or pane it is in — takes a told story,
//! moves a story watermark on a person's key, and computes the one deadline
//! the host's timer arms ([`deadline`]) and the tick that timer runs.
//!
//! PRESENCE NEVER RE-GRIDS A PANE (owner surfaces, 2026-10-16 §5): there is
//! no chrome row here to commit, so nothing a refresh does can resize a PTY.
//! The border is an overlay and the pill lives in the titlebar.
//!
//! Nothing here reads a clock, owns a timer or knows a window system: what
//! only a host can do comes in through [`Desk`], and the per-session state is
//! [`Table`]. A quiet desktop arms no deadline.

use std::collections::HashMap;

use super::pill::{Pill, PillAct, PillKind};
use super::{
    Facts, Host, Level, NeedSeverity, Rim, Slot, StopCause, StoryVerb, View, pill_due, pill_text,
    pill_words,
};
use crate::Instant;

/// The longest need text a pill carries, in cells: the words clip at the
/// painter's width before this. Public so a host that composes a need's
/// words with a tail of its own (`aterm-gui`'s `presence::need_fact`, a
/// harness's `(1 more need)`) clips them before this cut, which then never
/// falls inside the tail.
pub const PILL_WHAT_CELLS: usize = 96;

/// The longest tab label a pill carries, in cells.
const PILL_LABEL_CELLS: usize = 32;

/// The per-session slots, keyed by the host's local session id, and when
/// each session's `ttl=` supervisor claim lapses.
#[derive(Debug)]
pub struct Table<V: Host> {
    slots: HashMap<u64, Slot<V>>,
    /// When each session's `ttl=` supervisor claim lapses: the timer wakes
    /// there, because a supervisor that dies writes nothing and its box must
    /// still reach the human.
    supervisor_until: HashMap<u64, Instant>,
}

impl<V: Host> Default for Table<V> {
    fn default() -> Self {
        Self {
            slots: HashMap::new(),
            supervisor_until: HashMap::new(),
        }
    }
}

impl<V: Host> Table<V> {
    /// The session's slot, once any refresh has touched it.
    #[must_use]
    pub fn slot(&self, session: u64) -> Option<&Slot<V>> {
        self.slots.get(&session)
    }

    /// Fold one refresh's facts into `session`'s slot (minted at `now` on
    /// its first refresh): `true` when anything the views read moved — and
    /// always on the slot's first sight, so a fresh session is projected.
    pub fn absorb(&mut self, session: u64, facts: Facts<V>, now: Instant) -> bool {
        let slot = self.slots.entry(session).or_insert_with(|| Slot::new(now));
        let first = slot.agent_seq_seen.is_none() && slot.shell.is_none();
        slot.absorb(facts, now) || first
    }

    /// Forget `session`: its slot and its supervisor claim.
    pub fn retire(&mut self, session: u64) {
        self.slots.remove(&session);
        self.supervisor_until.remove(&session);
    }

    /// One told point on `session`'s slot (minted at `now` if none): its seq.
    fn tell(&mut self, session: u64, verb: StoryVerb, now: Instant) -> u64 {
        self.slots
            .entry(session)
            .or_insert_with(|| Slot::new(now))
            .tell(verb, now)
    }

    /// The sessions whose cooperative lease has lapsed by `now`.
    fn lapsed_leases(&self, now: Instant) -> Vec<u64> {
        self.slots
            .iter()
            .filter(|(_, slot)| slot.lease_until.is_some_and(|d| d <= now))
            .map(|(session, _)| *session)
            .collect()
    }

    /// The sessions whose `ttl=` supervisor claim has lapsed by `now`.
    fn lapsed_supervisors(&self, now: Instant) -> Vec<u64> {
        self.supervisor_until
            .iter()
            .filter(|(_, at)| **at <= now)
            .map(|(session, _)| *session)
            .collect()
    }

    /// The earliest lease or supervisor lapse, never before `now`. A
    /// supervisor's is the later of the lapse last read into the table and
    /// the claim's live one (`live`, [`Desk::supervisor_lapse`]): a renewal
    /// is no change (`meta set supervisor` by the holder it names posts no
    /// wake), so a deadline held at the lapse read before it woke the loop
    /// once a lease only to find the claim renewed — every 70 to 90 s for
    /// each supervised tab of a quiet desktop (the Together run, 2026-10-16).
    /// One whose holder died fires at its last renewal's lapse, and the tick
    /// re-reads it from the table's copy, which is never later.
    ///
    /// Read live AT EACH TURN of the host's loop, which is when it folds this
    /// deadline: a renewal moves the armed timer only at the next turn after
    /// it. aterm's own harness turns the loop every step (its `status` read
    /// of the tab, every 20 s, well inside a lease), so a claim it keeps
    /// renewed never fires. A holder that renews with no turn between two
    /// renewals still has its timer fire once a lease, at the lapse last
    /// read; the tick finds the claim renewed (the host's lapse re-checks the
    /// live expiry), lapses nothing, and re-arms — one wake that renders
    /// nothing.
    fn deadline(&self, now: Instant, live: impl Fn(u64) -> Option<Instant>) -> Option<Instant> {
        let leases = self.slots.values().filter_map(|slot| slot.lease_until);
        let supervisors = self
            .supervisor_until
            .iter()
            .map(|(session, at)| live(*session).map_or(*at, |lapse| lapse.max(*at)));
        leases.chain(supervisors).map(|d| d.max(now)).min()
    }
}

/// One terminal session a window shows ([`Desk::shown`]): in which tab, and
/// whether that tab is the window's front one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shown {
    /// The session's local id.
    pub session: u64,
    /// Its tab's index in the window, 0-based.
    pub tab: usize,
    /// That tab is the window's front tab.
    pub front_tab: bool,
}

/// A window's most pressing session ([`Desk::pressing_session`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pressing {
    /// The session.
    pub session: u64,
    /// Its tab's index, 0-based.
    pub tab: usize,
    /// Its tab is the window's front tab.
    pub front_tab: bool,
    /// How many other sessions the window shows need the person.
    pub more: usize,
}

/// What the presence driver asks of its host (the macOS App): every method is
/// a read of its state or one effect on it, and none decides anything the
/// driver decides.
pub trait Desk {
    /// The host's presence seam.
    type V: Host;
    /// A window's id.
    type Win: Copy + Eq;

    /// The host's clock, read where a refresh starts.
    fn clock(&self) -> Instant;
    /// The per-session state.
    fn table(&self) -> &Table<Self::V>;
    /// The per-session state, to change.
    fn table_mut(&mut self) -> &mut Table<Self::V>;
    /// Visit every window's view, in the host's window order.
    fn each_view(&self, f: &mut dyn FnMut(Self::Win, &View));
    /// One window's view.
    fn view_of(&self, w: Self::Win) -> Option<&View>;
    /// One window's view, to change.
    fn view_of_mut(&mut self, w: Self::Win) -> Option<&mut View>;
    /// The session a window shows in front (its focused tab's focused pane).
    fn front_session(&self, w: Self::Win) -> Option<u64>;
    /// Every terminal session `w` shows, any tab, any pane, in tab order.
    fn shown(&self, w: Self::Win) -> Vec<Shown>;
    /// Every window showing `session`: in front first, then in any tab.
    fn windows_showing(&self, session: u64) -> Vec<Self::Win>;
    /// Whether the host still holds `session`.
    fn holds_session(&self, session: u64) -> bool;
    /// Read `session`'s facts (`None`: the host no longer holds it). The
    /// need in them is the host's PUBLISHED verdict, which
    /// [`Self::herald`] has just published.
    fn sense(&self, session: u64) -> Option<Facts<Self::V>>;
    /// When `session`'s `ttl=` supervisor claim lapses on this clock, if it
    /// has one. Read at every refresh and by every [`deadline`] (once per
    /// supervised session): one leaf read, no lock held across it.
    fn supervisor_lapse(&self, session: u64, now: Instant) -> Option<Instant>;
    /// Whether someone can see `w`'s pill (a window on screen, not occluded,
    /// minimized or headless): its age arms a deadline only then.
    fn pill_visible(&self, w: Self::Win) -> bool;
    /// The View menu's switch: needs shown in windows (`[presence] rim`).
    /// Off, no border and no pill; the marks and the wire stay.
    fn needs_shown(&self) -> bool;
    /// The tab label the host gives `session` (its role, else its user
    /// title, else its program's title).
    fn label_of(&self, session: u64) -> String;
    /// Whether the host's restart act would be carried for `session` now.
    fn restart_offered(&self, session: u64) -> bool;
    /// `session`'s global id, as `chrome` prints it.
    fn sid_of(&self, session: u64) -> Option<String>;
    /// When the host owes a notification its rate limit allows (`now` when
    /// one is owed already).
    fn herald_deadline(&self, now: Instant) -> Option<Instant>;
    /// Called as `w` is re-projected, before its view changes: the host's
    /// per-window side channel (the macOS host publishes the front window's
    /// hold for its menu's synchronous validation).
    fn projecting(&mut self, w: Self::Win);
    /// Ask for a repaint of `w`.
    fn request_redraw(&self, w: Self::Win);
    /// Re-stamp the tab chips (and push the pill) of these windows.
    fn restamp_chips(&mut self, windows: Vec<Self::Win>);
    /// Push these windows' pills alone, whose age moved and nothing else: no
    /// chip is re-stamped and no strip re-planned while the pill keeps its
    /// width ([`super::PillWords::room`]). Returns the windows whose own frame
    /// draws the pill (an in-grid strip carries it): only their view's seed
    /// moves and only they repaint, so a pill the titlebar draws re-presents
    /// no frame.
    fn push_pill(&mut self, windows: Vec<Self::Win>) -> Vec<Self::Win>;
    /// Publish `session`'s need and let the herald look at it (it dedups by
    /// transition).
    fn herald(&mut self, session: u64);
    /// Post every owed notification the rate limit now allows.
    fn herald_owed(&mut self, now: Instant);
    /// Remove and record `session`'s lapsed supervisor claim.
    fn lapse_supervisor(&mut self, session: u64);
    /// Forget a session the host no longer holds (its slot, claim and
    /// herald memory).
    fn forget(&mut self, session: u64);

    /// THE WINDOW'S MOST PRESSING SESSION ([`pressing`] over [`Self::shown`]).
    fn pressing_session(&self, w: Self::Win) -> Option<Pressing> {
        pressing(self.table(), &self.shown(w), self.front_session(w))
    }

    /// The window's next need after `named` ([`next_need`] over
    /// [`Self::shown`]): where the pill's `+N` opens the host's Overview.
    fn next_need_after(&self, w: Self::Win, named: u64) -> Option<u64> {
        next_need(self.table(), &self.shown(w), self.front_session(w), named)
    }
}

/// The sessions `shown` that need the person, most pressing first by the
/// need alone — a stuck need before a decide one, then the oldest, then tab
/// order — each once, at its first place. No front-session term: this is the
/// order the pill's `+N` walks, which must not reorder as it moves the front.
fn needing_order<V: Host>(table: &Table<V>, shown: &[Shown]) -> Vec<u64> {
    let mut needing: Vec<(Shown, &Slot<V>)> = Vec::new();
    for s in shown {
        if needing.iter().any(|(n, _)| n.session == s.session) {
            continue;
        }
        if let Some(slot) = table.slot(s.session).filter(|slot| slot.need.is_some()) {
            needing.push((*s, slot));
        }
    }
    needing.sort_by_key(|(s, slot)| (rank(slot.severity()), slot.need_since, s.tab));
    needing.into_iter().map(|(s, _)| s.session).collect()
}

/// A need's rank: stuck first, then decide, then none.
const fn rank(severity: Option<NeedSeverity>) -> u8 {
    match severity {
        Some(NeedSeverity::Stuck) => 0,
        Some(NeedSeverity::Decide) => 1,
        None => 2,
    }
}

/// WHERE THE PILL'S `+N` POINTS: the window's next need after the one in
/// front — the front session where it needs the person, else `named` (the
/// session the pill names) — in [`needing_order`], wrapping round, so each
/// call lands on another of the window's needs and calls walk them all; the
/// host opens its Overview there. `None` when no other session needs
/// anything.
#[must_use]
pub fn next_need<V: Host>(
    table: &Table<V>,
    shown: &[Shown],
    front: Option<u64>,
    named: u64,
) -> Option<u64> {
    let order = needing_order(table, shown);
    let here = front.filter(|f| order.contains(f)).unwrap_or(named);
    let at = order.iter().position(|s| *s == here)?;
    let next = order[(at + 1) % order.len()];
    (next != here).then_some(next)
}

/// The most pressing of the sessions `shown`: a stuck need before a decide
/// one; within a severity the front session (`front`) first, then the
/// oldest need ([`Slot::need_since`]), then tab order. `more` counts the
/// other sessions with a need. `None` when none of them needs anything. A
/// session shown twice counts once, at its first place.
#[must_use]
pub fn pressing<V: Host>(
    table: &Table<V>,
    shown: &[Shown],
    front: Option<u64>,
) -> Option<Pressing> {
    let mut seen: Vec<u64> = Vec::with_capacity(shown.len());
    let mut needing: Vec<(Shown, &Slot<V>)> = Vec::new();
    for s in shown {
        if seen.contains(&s.session) {
            continue;
        }
        seen.push(s.session);
        if let Some(slot) = table.slot(s.session).filter(|slot| slot.need.is_some()) {
            needing.push((*s, slot));
        }
    }
    let more = needing.len().saturating_sub(1);
    needing
        .into_iter()
        .min_by_key(|(s, slot)| {
            (
                rank(slot.severity()),
                Some(s.session) != front,
                slot.need_since,
                s.tab,
            )
        })
        .map(|(s, _)| Pressing {
            session: s.session,
            tab: s.tab,
            front_tab: s.front_tab,
            more,
        })
}

/// Every window, in the host's order.
fn windows<D: Desk>(d: &D) -> Vec<D::Win> {
    let mut out = Vec::new();
    d.each_view(&mut |w, _| out.push(w));
    out
}

/// A fact about `session` changed: publish its need, re-read it, fold it in,
/// and project every window that shows it.
pub fn refresh_session<D: Desk>(d: &mut D, session: u64) {
    let now = d.clock();
    // Read at every refresh — a `meta set supervisor` that changes the
    // claim refreshes — so a cleared claim drops the deadline. A renewal
    // refreshes nothing; the deadline reads its lapse live
    // ([`Table::deadline`]).
    match d.supervisor_lapse(session, now) {
        Some(at) => {
            d.table_mut().supervisor_until.insert(session, at);
        }
        None => {
            d.table_mut().supervisor_until.remove(&session);
        }
    }
    // The need is published here, before the facts read it back; the human
    // channel rides the same change rate and dedups by transition itself.
    d.herald(session);
    let Some(facts) = d.sense(session) else {
        return;
    };
    if !d.table_mut().absorb(session, facts, now) {
        return;
    }
    let windows = d.windows_showing(session);
    for w in &windows {
        project_window(d, *w);
    }
    // The chips, and the pill the strip carries: every strip showing the
    // session re-stamps.
    d.restamp_chips(windows);
}

/// What `w` shows: the border, the session it is lit for and the pill. The
/// most pressing need first; with none, the person's own hold on the front
/// session; else nothing. The View switch off shows nothing.
fn compose<D: Desk>(d: &D, w: D::Win) -> (Rim, Option<u64>, Option<Pill>) {
    if !d.needs_shown() {
        return (Rim::None, None, None);
    }
    if let Some(p) = d.pressing_session(w)
        && let Some(slot) = d.table().slot(p.session)
        && let Some(need) = slot.need.as_ref()
    {
        let severity = need.severity;
        // A stall's one remedy, only on the tab in front: a background tab's
        // pill goes there first. A frozen program's is the restart, offered
        // where the host would carry it; a stopped job's is no restart (the
        // act answers `not-stalled` for it) but the stall's own words'
        // `signal cont`, which every host with job control carries.
        let act = match need.cause {
            _ if !p.front_tab || severity != NeedSeverity::Stuck => None,
            StopCause::Frozen if d.restart_offered(p.session) => Some(PillAct::Restart),
            StopCause::Suspended => Some(PillAct::Resume),
            StopCause::Frozen | StopCause::Wall => None,
        };
        // The session in front is named already; any other — a background
        // tab, or another pane of the tab in front — by its label.
        let label = if p.front_tab && d.front_session(w) == Some(p.session) {
            String::new()
        } else {
            pill_text::<D::V>(&d.label_of(p.session), PILL_LABEL_CELLS)
        };
        let pill = Pill {
            kind: PillKind::Need(severity),
            session: p.session,
            label,
            tab: (!p.front_tab).then_some(p.tab + 1),
            what: pill_text::<D::V>(&need.what, PILL_WHAT_CELLS),
            // Only a background tab's pill prints its age, so only its age
            // arms a tick ([`deadline`]): the tab in front is in view.
            since: (!p.front_tab).then_some(slot.need_since),
            act,
            more: p.more,
        };
        return (Rim::of(severity), Some(p.session), Some(pill));
    }
    if let Some(front) = d.front_session(w)
        && d.table()
            .slot(front)
            .is_some_and(|slot| slot.held_by_person)
    {
        let pill = Pill {
            kind: PillKind::Yours,
            session: front,
            label: String::new(),
            tab: None,
            what: String::new(),
            since: None,
            act: Some(PillAct::HandBack),
            more: 0,
        };
        return (Rim::Yours, Some(front), Some(pill));
    }
    (Rim::None, None, None)
}

/// Project `w` ([`compose`]) onto its view: whether anything the painter
/// reads moved (a repaint is asked for then).
fn project_window<D: Desk>(d: &mut D, w: D::Win) -> bool {
    let (rim, pressing, pill) = compose(d, w);
    d.projecting(w);
    let moved = d
        .view_of_mut(w)
        .is_some_and(|v| v.show(rim, pressing, pill));
    if moved {
        d.request_redraw(w);
    }
    moved
}

/// Re-project `w` (a tab switch, a focus change, a restore): its border and
/// pill follow, and its strip re-stamps where they moved.
pub fn refresh_window<D: Desk>(d: &mut D, w: D::Win) {
    if project_window(d, w) {
        d.restamp_chips(vec![w]);
    }
}

/// Every window: the tab-switch / focus-change / restore funnel.
pub fn refresh_all<D: Desk>(d: &mut D) {
    let moved: Vec<D::Win> = windows(d)
        .into_iter()
        .filter(|w| project_window(d, *w))
        .collect();
    if !moved.is_empty() {
        d.restamp_chips(moved);
    }
}

/// A person's key (or press) in `w` while `session` was its front: if the
/// session is calm, its story is read here (the watermark moves to its
/// newest point), which `status level=story` reads. Never on focus alone,
/// never on a timer, never while anything is still happening. Nothing on
/// the glass moves: the story has no row.
pub fn human_acted_on<D: Desk>(d: &mut D, w: D::Win, session: u64) {
    let Some(seq) = d
        .table()
        .slot(session)
        .filter(|slot| slot.calm())
        .map(|slot| slot.story_seq)
    else {
        return;
    };
    if let Some(v) = d.view_of_mut(w) {
        v.watermarks.insert(session, seq);
    }
}

/// [`human_acted_on`] for `w`'s front session.
pub fn human_acted<D: Desk>(d: &mut D, w: D::Win) {
    if let Some(session) = d.front_session(w) {
        human_acted_on(d, w, session);
    }
}

/// A told story point landed for `session` (`aterm ctl story`): the slot is
/// brought current first, then the point is noted (`status story=`). `None`
/// for a session the host no longer holds; else the point's seq.
pub fn tell<D: Desk>(d: &mut D, session: u64, verb: StoryVerb) -> Option<u64> {
    if !d.holds_session(session) {
        return None;
    }
    refresh_session(d, session);
    let now = d.clock();
    Some(d.table_mut().tell(session, verb, now))
}

/// The one instant the host's timer arms: a visible pill's next age figure
/// (one never ticked is due now), a cooperative lease's or a supervisor
/// claim's lapse, and the host's owed notification. `None` on a quiet
/// desktop.
pub fn deadline<D: Desk>(d: &D, now: Instant) -> Option<Instant> {
    let mut deadline: Option<Instant> = None;
    let mut fold = |x: Instant| {
        if deadline.is_none_or(|cur| x < cur) {
            deadline = Some(x);
        }
    };
    d.each_view(&mut |w, v| {
        // An occluded, minimized or headless window's pill is read by
        // nobody, so its age arms nothing (`chrome` composes it at the read).
        if v.pill.as_ref().is_some_and(|p| p.since.is_some()) && d.pill_visible(w) {
            fold(v.pill_due.map_or(now, |x| x.max(now)));
        }
    });
    // Neither a lease's lapse nor a supervisor's posts a wake of its own.
    if let Some(x) = d
        .table()
        .deadline(now, |session| d.supervisor_lapse(session, now))
    {
        fold(x);
    }
    if let Some(x) = d.herald_deadline(now) {
        fold(x);
    }
    deadline
}

/// The timer's tick at `now`: a lapsed supervisor claim is removed and its
/// session re-read, owed notifications the rate limit allows are posted, a
/// lapsed cooperative lease is re-read, and every visible pill whose age
/// figure moved is pushed alone ([`Desk::push_pill`]). Returns the windows
/// that need a repaint: those whose own frame draws the pill.
pub fn tick<D: Desk>(d: &mut D, now: Instant) -> Vec<D::Win> {
    let mut out = Vec::new();
    for session in d.table().lapsed_supervisors(now) {
        if !d.holds_session(session) {
            d.forget(session);
            continue;
        }
        d.lapse_supervisor(session);
        // Re-reads the claim: a lapsed one is gone (the deadline with it), a
        // renewed one re-arms at its new expiry.
        refresh_session(d, session);
    }
    d.herald_owed(now);
    for session in d.table().lapsed_leases(now) {
        if !d.holds_session(session) {
            // Dropped rather than re-armed every tick.
            d.forget(session);
            continue;
        }
        refresh_session(d, session);
        if let Some(slot) = d.table_mut().slots.get_mut(&session)
            && slot.lease_until.is_some_and(|x| x <= now)
        {
            slot.lease_until = None;
        }
    }
    let mut aged = Vec::new();
    for w in windows(d) {
        let visible = d.pill_visible(w);
        let Some(v) = d.view_of_mut(w) else {
            continue;
        };
        let Some(since) = v.pill.as_ref().and_then(|p| p.since) else {
            continue;
        };
        if !visible || v.pill_due.is_some_and(|x| x > now) {
            continue;
        }
        let first = v.pill_due.is_none();
        v.pill_due = Some(pill_due(since, now));
        if !first {
            aged.push(w);
        }
    }
    if !aged.is_empty() {
        // The age moved and nothing else: the pill alone is pushed, and only
        // a frame that draws it (the in-grid strip's) moves its key.
        for w in d.push_pill(aged) {
            if let Some(v) = d.view_of_mut(w) {
                v.seed = v.seed.wrapping_add(1).max(1);
                out.push(w);
            }
        }
    }
    out
}

// The wire strings: what the `status` and `chrome` verbs print of presence,
// composed here so every host prints the same bytes. The percent codec stays
// the host's ([`Host::pct_encode`]).

/// The highest story watermark any window holds for `session` (0: no window
/// has read its story).
fn read_mark<D: Desk>(d: &D, session: u64) -> u64 {
    let mut mark = 0;
    d.each_view(&mut |_, v| mark = mark.max(v.watermark(session)));
    mark
}

/// A session's level as `status level=` reports it: read against the highest
/// watermark any window holds for the session, so a story read in any window
/// is read whichever tab is in front now. [`Level::Quiet`] for a session no
/// refresh has touched.
#[must_use]
pub fn session_level<D: Desk>(d: &D, session: u64) -> Level {
    d.table()
        .slot(session)
        .map_or(Level::Quiet, |slot| slot.level(read_mark(d, session)))
}

/// The additive presence `status` fields: `hand=<token> hand_pid=<pid|->
/// level=<level> story=<n> why=<causes>` ([`Slot::why`]), plain reads of the
/// slot the refreshes keep current; `hand=- hand_pid=- level=quiet story=0
/// why=-` for a session no refresh has touched yet. `hand_pid=` is the
/// kernel-attested pid of a drive lease's taker ([`Hand::pid_wire`]).
#[must_use]
pub fn status_tail<D: Desk>(d: &D, session: u64) -> String {
    let level = session_level(d, session).wire();
    match d.table().slot(session) {
        Some(slot) => format!(
            "hand={} hand_pid={} level={level} story={} why={}",
            slot.hand.wire::<D::V>(),
            slot.hand.pid_wire(),
            slot.story_seq,
            slot.why(read_mark(d, session))
        ),
        None => format!("hand=- hand_pid=- level={level} story=0 why=-"),
    }
}

/// `chrome`'s presence line for the host's front window `front`: `presence
/// rim=<none|stuck|decide|yours> pressing=<@sid|-> pill="<the pill's
/// sentence>"`, what the person sees. The pill is composed at the read (its
/// age as of now) and printed whole, unclipped; `"` and `\` are escaped so
/// the line stays one parseable record. No window, or a quiet one, prints
/// `rim=none pressing=- pill=""`.
#[must_use]
pub fn chrome_line<D: Desk>(d: &D, front: Option<D::Win>) -> String {
    let now = d.clock();
    let view = front.and_then(|w| d.view_of(w));
    let rim = view.map_or(Rim::None, |v| v.rim).wire();
    let pressing = view
        .and_then(|v| v.pressing)
        .and_then(|s| d.sid_of(s))
        .map_or_else(|| "-".to_string(), |sid| format!("@{sid}"));
    let pill = view
        .and_then(|v| v.pill.as_ref())
        .map(|p| pill_words(p, now).text())
        .unwrap_or_default();
    format!(
        "presence rim={rim} pressing={pressing} pill={}",
        quoted(&pill)
    )
}

/// `s` in double quotes, with `"` and `\` backslash-escaped.
fn quoted(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
