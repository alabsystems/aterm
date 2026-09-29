// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE PRESENCE DRIVER (design ruling 353) — the sequencing every host ran
//! for itself until round 31, as the band's own driver (`crate::drive`,
//! ruling 336) did before it: fold a session's facts into its slot and
//! re-project every window that shows it, project one window's focused
//! session onto its view and commit the row to its geometry, the fold law's
//! decision, the edge ripple and the choice pulse, a told story, the one
//! deadline the host's timer arms and the tick that timer runs. The owner's
//! ask (2026-09-21): *"design the logic in aterm core and then keep the osx
//! layer lightweight so that we can make this cross platform."*
//!
//! Nothing here reads a clock, owns a timer or knows a window system. What
//! only a host can do comes in through [`Desk`]: the clock, the window walk,
//! the facts it senses, whether a band is on screen, the motion policy, the
//! re-grid a committed row count asks for, the repaint request, the tab
//! chips, the menu-bar herald and the chime. The per-session state the
//! driver keeps is [`Table`].
//!
//! No wake is added: every instant [`deadline`] returns is a ripple's step,
//! a told point's flash, a row's own `since` clock (only while its band is on
//! screen), a row's held fold ([`FOLD_QUIET`]) or a birth a lifted hold left
//! owed (`rows_deadline`), a cooperative lease's lapse,
//! a supervisor claim's lapse or the host's owed notification. A quiet
//! desktop returns `None`.

use std::collections::HashMap;

use super::{
    FOLD_LOOK_CAP, FOLD_QUIET, Facts, Host, LeaseMark, Level, Rim, Slot, StoryVerb, View, Words,
    text_cols, words, words_step,
};
use crate::Instant;

/// The per-session slots, keyed by the host's local session id, and when
/// each session's `ttl=` supervisor claim lapses.
#[derive(Debug)]
pub struct Table<V: Host> {
    slots: HashMap<u64, Slot<V>>,
    /// When each session's `ttl=` supervisor claim lapses — the timer wakes
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
    fn tell(&mut self, session: u64, verb: StoryVerb, text: &str, now: Instant) -> u64 {
        self.slots
            .entry(session)
            .or_insert_with(|| Slot::new(now))
            .tell(verb, text, now)
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

    /// The earliest lease or supervisor lapse, never before `now`.
    fn deadline(&self, now: Instant) -> Option<Instant> {
        let leases = self.slots.values().filter_map(|slot| slot.lease_until);
        leases
            .chain(self.supervisor_until.values().copied())
            .map(|d| d.max(now))
            .min()
    }
}

/// What the presence driver asks of its host. Implemented on the host's own
/// state (the macOS App): every method is a read of it or one effect on it,
/// and none decides anything the driver decides.
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
    /// The session a window shows in front (its focused tab's).
    fn front_session(&self, w: Self::Win) -> Option<u64>;
    /// Every window showing `session`: in front first, then in any tab.
    fn windows_showing(&self, session: u64) -> Vec<Self::Win>;
    /// Whether the host still holds `session`.
    fn holds_session(&self, session: u64) -> bool;
    /// Read `session`'s facts (`None`: the host no longer holds it).
    fn sense(&self, session: u64) -> Option<Facts<Self::V>>;
    /// When `session`'s `ttl=` supervisor claim lapses on this clock, if it
    /// has one.
    fn supervisor_lapse(&self, session: u64, now: Instant) -> Option<Instant>;
    /// Whether a window's band is on screen (not occluded, minimized or
    /// headless): the row's `since` text arms a deadline only then.
    fn band_visible(&self, w: Self::Win) -> bool;
    /// Whether a window holds the OS key focus.
    fn key_window(&self, w: Self::Win) -> bool;
    /// Whether the motion policies let a ripple start on `w` (`focused`:
    /// whether it counts as focused for them).
    fn ripple_allowed(&self, w: Self::Win, focused: bool) -> bool;
    /// The View menu's presence BAND toggle.
    fn band_toggle(&self) -> bool;
    /// The View menu's presence RIM toggle.
    fn rim_toggle(&self) -> bool;
    /// A seamless handoff freezes every window's row count.
    fn rows_frozen(&self) -> bool;
    /// THE DRIVER'S GEOMETRY: whether a driver may still type into any
    /// session `w` shows (a `turn` before its settle, a live drive lease).
    /// The window's row count holds meanwhile — a re-grid resizes every pane,
    /// the program repaints on the `SIGWINCH`, and a driver's `if-gen=` fence
    /// would read its own lease's re-grid as a change (measured 2026-09-27:
    /// the live upgrade's notice refused on 121 visits of 121). The row
    /// catches up on the wake that lifts it ([`Facts::typing`]).
    fn driver_may_type(&self, w: Self::Win) -> bool {
        let _ = w;
        false
    }
    /// THE LIVE LEASE of `session`, as [`Facts::lease`] reduces it — read
    /// again now, not the slot's copy. The row's commit holds while it
    /// differs from the mark the front session's words were formed under
    /// (`commit_rows`), and a window refreshed meanwhile re-senses the
    /// session first (`refresh_window`). `None`: the host keeps no lease, and
    /// only [`Facts::typing`] and [`Self::driver_may_type`] hold.
    fn lease_mark(&self, session: u64) -> Option<LeaseMark> {
        let _ = session;
        None
    }
    /// When a control client was last handed the screen generation of a
    /// session `w` shows (`status gen=`, `text --json`) — a read an `if-gen=`
    /// fence may name; `None` when none was. A pending fold waits
    /// [`FOLD_QUIET`] past it (bounded by [`FOLD_LOOK_CAP`]), so the fold's
    /// re-grid never lands between a driver's read and its act.
    fn looked_at(&self, w: Self::Win) -> Option<Instant> {
        let _ = w;
        None
    }
    /// A window's terminal rows and whether it has an OS window (a headless
    /// window takes the row whatever its height); `None` for no window.
    fn grid_rows(&self, w: Self::Win) -> Option<(u16, bool)>;
    /// When the host owes a notification its rate limit allows (`now` when
    /// one is owed already).
    fn herald_deadline(&self, now: Instant) -> Option<Instant>;
    /// Called as `w` is re-projected, before its view changes: the host's
    /// per-window side channel (the macOS host publishes the front window's
    /// hold for its menu's synchronous validation).
    fn projecting(&mut self, w: Self::Win);
    /// The row count of `w`'s view moved: pay the one re-grid (a PTY resize).
    fn regrid(&mut self, w: Self::Win);
    /// Ask for a repaint of `w`.
    fn request_redraw(&self, w: Self::Win);
    /// Re-stamp the tab chips of these windows.
    fn restamp_chips(&mut self, windows: Vec<Self::Win>);
    /// The menu-bar herald's look at `session` (it dedups by transition).
    fn herald(&mut self, session: u64);
    /// Post every owed notification the rate limit now allows.
    fn herald_owed(&mut self, now: Instant);
    /// Remove and record `session`'s lapsed supervisor claim.
    fn lapse_supervisor(&mut self, session: u64);
    /// Forget a session the host no longer holds (its slot, claim and
    /// herald memory).
    fn forget(&mut self, session: u64);
    /// The choice chime (a supervisor answered a question box).
    fn chime(&mut self, now: Instant);
}

/// Every window, in the host's order.
fn windows<D: Desk>(d: &D) -> Vec<D::Win> {
    let mut out = Vec::new();
    d.each_view(&mut |w, _| out.push(w));
    out
}

/// A fact about `session` changed: re-read it, fold it in, and project it
/// onto every window that shows it. `submitted` marks a turn's submit
/// keypress — the ripple's edge.
pub fn refresh_session<D: Desk>(d: &mut D, session: u64, submitted: bool) {
    let now = d.clock();
    // Read at every refresh — a `meta set supervisor` refreshes — so a
    // renewal moves the deadline and a cleared claim drops it.
    match d.supervisor_lapse(session, now) {
        Some(at) => {
            d.table_mut().supervisor_until.insert(session, at);
        }
        None => {
            d.table_mut().supervisor_until.remove(&session);
        }
    }
    // The human channel rides every refresh — the same change rate — and
    // dedups by transition itself.
    d.herald(session);
    let Some(facts) = d.sense(session) else {
        return;
    };
    let changed = d.table_mut().absorb(session, facts, now);
    if !changed && !submitted {
        return;
    }
    let windows = d.windows_showing(session);
    for w in &windows {
        if d.front_session(*w) == Some(session) && submitted {
            ripple(d, *w, now);
        }
        project_window(d, *w);
    }
    // The chips: every strip showing the session re-stamps its levels.
    d.restamp_chips(windows);
}

/// Start the 300 ms edge ripple on `w` — unless motion is reduced, where the
/// amplitude is 0 and the ripple never starts (the same image). The
/// turn-submit ripple keeps the unfocused-window rule every decorative
/// motion has.
pub fn ripple<D: Desk>(d: &mut D, w: D::Win, now: Instant) {
    start_ripple(d, w, now, false);
}

/// Start the CHOICE PULSE on `w`: the same 300 ms edge flash, painted in the
/// story tone and shown even on a window with no rim. It IS a rim flash, so
/// the rim toggle off means none; it is not held to the OS key focus — its
/// point is to show that a question in a window the person is NOT in was
/// answered. Only reduced motion and serious mode stop it.
pub fn pulse<D: Desk>(d: &mut D, w: D::Win, now: Instant) {
    if d.rim_toggle() {
        start_ripple(d, w, now, true);
    }
}

fn start_ripple<D: Desk>(d: &mut D, w: D::Win, now: Instant, chose: bool) {
    let focused = chose || d.key_window(w);
    if !d.ripple_allowed(w, focused) {
        return;
    }
    let started = d.view_of_mut(w).is_some_and(|v| {
        v.ripple_at = Some(now);
        v.ripple_chose = chose;
        true
    });
    if started {
        d.request_redraw(w);
    }
}

/// What a window shows of `slot` read at `watermark`: the level, the rim and
/// the row's words. An attention the message band already tells is not
/// repeated here ([`Slot::shown_level`]); the toggles hide the row (`band`)
/// or the rim (`rim`) and leave the level alone.
fn project<V: Host>(
    slot: Option<&Slot<V>>,
    watermark: u64,
    band: bool,
    rim: bool,
    now: Instant,
) -> (Level, Rim, Option<Words>) {
    let (level, words) = match slot {
        Some(slot) => {
            let level = slot.shown_level(watermark);
            let words = level.shows_row().then(|| words(slot, now, watermark));
            (level, words)
        }
        None => (Level::Quiet, None),
    };
    let words = if band { words } else { None };
    let rim = if rim { level.rim() } else { Rim::None };
    (level, rim, words)
}

/// Project `w`'s front session onto its view — level, rim, words and the
/// row's existence — and commit the row count to its geometry. Asks for a
/// repaint when anything the painter reads moved or the row folded.
///
/// A front session whose lease MOVED since its slot was sensed
/// ([`lease_moved`]) is re-sensed first — the whole [`refresh_session`], so
/// every window showing it is re-projected from the one read — rather than
/// left to the wake that moved it: a refresh (a tick, a focus change, a tab
/// switch) never projects words from a lease that is gone, and the row a
/// moved lease holds catches up on the first refresh after the move, the
/// release's own wake or any other. Once: a lease that moves again under
/// that read is held by `commit_rows`, and its own wake follows.
pub fn refresh_window<D: Desk>(d: &mut D, w: D::Win) {
    if let Some(session) = d.front_session(w).filter(|s| lease_moved(d, *s)) {
        refresh_session(d, session, false);
    }
    project_window(d, w);
}

/// [`refresh_window`] without the re-sense: [`refresh_session`]'s own
/// projection of the windows showing the session it has just sensed.
fn project_window<D: Desk>(d: &mut D, w: D::Win) {
    let now = d.clock();
    refresh_window_at(d, w, now, false);
}

/// [`refresh_window`] at `now` (the tick's instant, so a held fold is judged
/// on the clock that woke for it); `read` marks a fold a person's read (the
/// fold law's keystroke), which waits only for the row's minimum life
/// ([`View::fold_read`]). A switch of the window's front session is a read
/// too.
fn refresh_window_at<D: Desk>(d: &mut D, w: D::Win, now: Instant, read: bool) {
    let session = d.front_session(w);
    let switched = d.view_of_mut(w).is_some_and(|v| {
        let switched = v.front.is_some_and(|f| Some(f) != session);
        v.front = session;
        switched
    });
    let watermark = session
        .and_then(|s| d.view_of(w).map(|v| v.watermark(s)))
        .unwrap_or(0);
    let slot = session.and_then(|s| d.table().slot(s));
    let (level, rim, words) = project(slot, watermark, d.band_toggle(), d.rim_toggle(), now);
    d.projecting(w);
    let moved = d
        .view_of_mut(w)
        .is_some_and(|v| v.show(level, rim, words, now));
    let folded = commit_rows(d, w, now, read || switched);
    if moved || folded {
        d.request_redraw(w);
    }
}

/// Every window: the tab-switch / focus-change / restore funnel.
pub fn refresh_all<D: Desk>(d: &mut D) {
    for w in windows(d) {
        refresh_window(d, w);
    }
}

/// Commit the row's existence to `w`'s geometry: `true` when the count moved
/// (and the host re-gridded — ONE PTY resize). Frozen mid-handoff, held while
/// a driver may type ([`Desk::driver_may_type`]), and yielding to the last
/// terminal row of a real window.
///
/// THE HOLD IS JUDGED ON THE WORDS' OWN SNAPSHOT TOO. The row's want is the
/// front session's words, formed from the facts its last refresh SENSED; the
/// host's [`Desk::driver_may_type`] reads the LIVE lease, later. A lease let go
/// between the two (measured 2026-09-28: `lease acquire` then `lease release`
/// under 0.1 ms apart, 3 runs of 10 on a live window) left words that still
/// said "driven" and a live read that said "nobody typing" — the row was born
/// for a hand already gone and folded again on the release's own wake, a
/// 24 → 23 → 24 re-grid inside 0.35 ms under a program on the alternate
/// screen. A `turn` ending its input and releasing its lease back to back
/// does the same with words that say "settling". So the words' own lease
/// holds as well: while the mark they were formed under ([`Slot::lease`])
/// differs from the host's live one ([`Desk::lease_mark`]) — any move of the
/// lease, which always posts a wake — and while it said a driver may type
/// ([`Slot::typing`], for a host that keeps no live mark). The wake that
/// moved it re-derives the want. The live read still holds a lease taken
/// after the sense. `DriverGeometry`'s `NoBirthForAGoneHand` and
/// `TheHeldRowCatchesUp` are the two halves (`aterm-spec`).
///
/// THE FOLD QUIET (ruling 394, proposed): a birth commits at once, a fold only
/// when [`fold_due`] says — a row born and folded inside a millisecond cost
/// two re-grids, and an alternate-screen program that repaints only on a
/// CHANGED size never repainted after the net-zero pair (measured
/// 2026-09-28). A want that returns first cancels the fold with no re-grid.
/// Two folds are not held at all: the WINDOW's (a real window that can no
/// longer afford the row gets its terminal row back now, as the band's D1
/// rule does) and the human's hiding the band (the View menu). `read` marks
/// the fold a person's read ([`View::fold_read`]). The quiet runs through a
/// hold: a fold whose want stayed away through a handoff or a driver's input
/// commits as the hold lifts.
///
/// THE TWO COMPOSE ON ONE RULE: a want formed under a lease that has since
/// moved ([`lease_moved`]) decides NOTHING — no birth, no fold, and no start
/// or cancel of the quiet: a pending fold keeps its clock (and a person's
/// read of it) until the wake the move posted re-senses the session and
/// judges the fresh want, and [`rows_deadline`] arms no instant meanwhile, so
/// the tick never spins on a fold it may not commit. The window's and the
/// band toggle's folds wait that one wake too: it is already posted. A
/// sensed [`Slot::typing`] is an ordinary hold, like the live one: the
/// quiet runs through it.
fn commit_rows<D: Desk>(d: &mut D, w: D::Win, now: Instant, read: bool) -> bool {
    let front = d.front_session(w);
    let moved = front.is_some_and(|s| lease_moved(d, s));
    let typing = front.is_some_and(|s| d.table().slot(s).is_some_and(|slot| slot.typing));
    let held = d.rows_frozen() || typing || d.driver_may_type(w);
    let band = d.band_toggle();
    let geometry = d.grid_rows(w);
    let looked = d.looked_at(w);
    let Some(v) = d.view_of_mut(w) else {
        return false;
    };
    let Some((grid_rows, windowed)) = geometry else {
        // No grid to commit to: no fold is pending either, so the deadline
        // never names an instant the tick cannot act on.
        v.fold_since = None;
        v.fold_read = false;
        return false;
    };
    if moved {
        // The want is a gone lease's: kept for the wake the move posted,
        // with a person's read of a fold already pending.
        if v.fold_since.is_some() {
            v.fold_read |= read;
        }
        return false;
    }
    let afford = afford(v, grid_rows);
    let want = row_want(v, (grid_rows, windowed));
    if want >= v.rows {
        v.fold_since = None;
        v.fold_read = false;
        if want == v.rows || held {
            return false;
        }
        v.born_at = Some(now);
    } else {
        v.fold_since.get_or_insert(now);
        v.fold_read |= read;
        if held {
            return false;
        }
        let window_took_it = windowed && afford < v.rows;
        let due = fold_due(v, looked).is_some_and(|x| now >= x);
        if !(due || !band || window_took_it) {
            return false;
        }
        v.fold_since = None;
        v.fold_read = false;
    }
    v.rows = want;
    d.regrid(w);
    true
}

/// Whether a window of `grid_rows` terminal rows can afford `v`'s row: `1`
/// while at least one terminal row would stay beside it.
fn afford(v: &View, grid_rows: u16) -> u16 {
    grid_rows.saturating_add(v.rows).saturating_sub(1).min(1)
}

/// The row count `v`'s words want on a grid of `(grid_rows, windowed)`: one
/// while it has words, none on a real window that cannot afford it (a
/// headless window takes the row whatever its height).
fn row_want(v: &View, (grid_rows, windowed): (u16, bool)) -> u16 {
    let want = u16::from(v.words.is_some());
    if windowed {
        want.min(afford(v, grid_rows))
    } else {
        want
    }
}

/// Whether `session`'s lease moved since its slot was sensed: the slot's
/// [`Slot::lease`] against the host's live [`Desk::lease_mark`]. A session
/// with no slot yet, or a host that keeps no live mark, never reads as moved.
fn lease_moved<D: Desk>(d: &D, session: u64) -> bool {
    match (d.table().slot(session), d.lease_mark(session)) {
        (Some(slot), Some(live)) => slot.lease != live,
        _ => false,
    }
}

/// Whether `w`'s front words were formed under a lease that has moved since
/// ([`lease_moved`]) or that said a driver may type ([`Slot::typing`]): the
/// words' own snapshot holds of [`commit_rows`], which [`rows_deadline`]
/// arms no instant through.
fn words_stale<D: Desk>(d: &D, w: D::Win) -> bool {
    d.front_session(w)
        .is_some_and(|s| lease_moved(d, s) || d.table().slot(s).is_some_and(|slot| slot.typing))
}

/// When `v`'s pending fold may commit; `None` with none pending.
///
/// * A person's READ ([`View::fold_read`]: the fold law's keystroke, a tab
///   switch) folds at once, once the row has stood [`FOLD_QUIET`] since its
///   birth — a person acting is not a flap, but a row a key folds
///   milliseconds after a told story bore it is the incident's pair.
/// * Any other fold waits [`FOLD_QUIET`] past the want's drop, and past the
///   last LOOK a driver took at the window's screen ([`Desk::looked_at`]),
///   the look's hold bounded by [`FOLD_LOOK_CAP`] past the drop: a fold
///   committed between a driver's read and its fenced act refuses the act.
fn fold_due(v: &View, looked: Option<Instant>) -> Option<Instant> {
    let since = v.fold_since?;
    if v.fold_read {
        return Some(v.born_at.map_or(since, |b| (b + FOLD_QUIET).max(since)));
    }
    let base = looked.filter(|x| *x > since).unwrap_or(since);
    Some((base + FOLD_QUIET).min(since + FOLD_LOOK_CAP))
}

/// When `w`'s row count owes a commit, never before `now`; `None` while its
/// count is held, or with nothing owed.
///
/// * A held FOLD is due at [`fold_due`].
/// * A BIRTH the words want that no hold stands against is owed `now`
///   (review of the integration, 2026-09-29). A birth is committed by the
///   projection that formed its want, and a hold turns that projection
///   away; nothing re-projects the window for it but a wake whose facts
///   moved. A hold that lifts without one strands it: a background tab's
///   lease that held the birth ([`Desk::driver_may_type`] reads every
///   session the window shows) is taken and let go before either of its
///   wakes runs, so both read facts identical to its slot and
///   [`refresh_session`] projects nothing — and the same for a lease taken
///   between a refresh's sense and its commit and let go before its wake.
///   The band's words clock caught it up a minute later on a visible
///   window, and never on a headless or occluded one. The tick commits it
///   instead. No spin: this excludes exactly [`commit_rows`]' holds, and
///   the tick re-projects the window before it commits.
///
/// The wake that lifts a hold the words themselves carry
/// ([`words_stale`]) re-projects the window; one that lifts a live hold is
/// what the birth's clause above catches when it does not.
fn rows_deadline<D: Desk>(d: &D, w: D::Win, v: &View, now: Instant) -> Option<Instant> {
    if d.rows_frozen() || d.driver_may_type(w) || words_stale(d, w) {
        return None;
    }
    if d.grid_rows(w).is_some_and(|g| row_want(v, g) > v.rows) {
        return Some(now);
    }
    Some(fold_due(v, d.looked_at(w))?.max(now))
}

/// THE FOLD LAW. The human acted (a key, a click) in `w`: if its front
/// session is CALM, the story is read — the watermark moves to the newest
/// point and the row folds. Never on focus alone, never on a timer, and
/// never while anything is still happening.
pub fn human_acted<D: Desk>(d: &mut D, w: D::Win) {
    let Some(session) = d.front_session(w) else {
        return;
    };
    human_acted_on(d, w, session);
}

/// THE FOLD LAW for an act that may move the front: the human acted in `w`
/// while `session` was its front — the story they SAW. A click presses the
/// layout the person saw first (ruling 369), and that press can bring
/// another session front (a click in another split pane) or none at all (a
/// capsule that opens Settings or an editor); the fold reads the session
/// that was front BEFORE it, never the one the press made front (ruling
/// 372). The same law as [`human_acted`] otherwise: only while calm, once.
pub fn human_acted_on<D: Desk>(d: &mut D, w: D::Win, session: u64) {
    // CALM IS JUDGED ON THE LIVE LEASE (review of the integration,
    // 2026-09-29). A lease taken whose wake has not run yet leaves the slot
    // reading calm: the key read the story (the watermark moved) while the
    // commit's moved-lease hold dropped the read, so once the lease ended the
    // blank row stood a fresh quiet — and with the wake handled first, the
    // same key read nothing. Re-sensing a moved lease first, as
    // [`refresh_window`] does, makes both orders one: a lease just taken is
    // not calm, and one just let go is judged as let go.
    if lease_moved(d, session) {
        refresh_session(d, session, false);
    }
    let Some(slot) = d.table().slot(session) else {
        return;
    };
    if !slot.calm() {
        return;
    }
    let seq = slot.story_seq;
    let Some(v) = d.view_of_mut(w) else {
        return;
    };
    // A row already blank whose fold is pending is still the keystroke's to
    // fold (review of ruling 394: the early return skipped it, so the blank
    // row stood out its quiet under the person's eyes).
    if v.watermark(session) == seq && v.words.is_none() && (v.fold_since.is_none() || v.fold_read) {
        return;
    }
    v.watermarks.insert(session, seq);
    // The keystroke is a READ, and the read folds the row: a person reading
    // a calm story is not a sub-millisecond flap (the fold quiet's cause) and
    // the row cannot come back from the story just read — once the row has
    // stood its minimum life ([`fold_due`]); until then the timer folds it.
    let now = d.clock();
    refresh_window_at(d, w, now, true);
}

/// A told story point landed for `session` (`aterm ctl story`): the slot is
/// brought current first (a story on a session no wake has touched must not
/// print a blank row), the point is noted, and every window showing the
/// session is re-projected. A [`StoryVerb::Chose`] pulses every window whose
/// FRONT tab shows the session (a background tab has its story dot, and a
/// rim flash would name the wrong tab) and chimes once. `None` for a session
/// the host no longer holds; else the point's seq.
pub fn tell<D: Desk>(d: &mut D, session: u64, verb: StoryVerb, text: &str) -> Option<u64> {
    if !d.holds_session(session) {
        return None;
    }
    refresh_session(d, session, false);
    let now = d.clock();
    let seq = d.table_mut().tell(session, verb, text, now);
    let showing = d.windows_showing(session);
    for w in &showing {
        refresh_window(d, *w);
    }
    if verb == StoryVerb::Chose {
        let fronts: Vec<D::Win> = windows(d)
            .into_iter()
            .filter(|w| d.front_session(*w) == Some(session))
            .collect();
        for w in fronts {
            pulse(d, w, now);
        }
        d.chime(now);
    }
    d.restamp_chips(showing);
    Some(seq)
}

/// The one instant the host's timer arms: a running ripple's next step, a
/// front session's told flash, an on-screen row's own `since` clock (a row
/// not yet ticked is due now; a window revealed after its figure moved
/// catches up at once), a row's held fold ([`FOLD_QUIET`]) or owed birth
/// (not while its count is held), a cooperative lease's or a supervisor
/// claim's lapse, and
/// the host's owed notification. `None` on a quiet desktop.
pub fn deadline<D: Desk>(d: &D, now: Instant) -> Option<Instant> {
    let mut deadline: Option<Instant> = None;
    let mut fold = |x: Instant| {
        if deadline.is_none_or(|cur| x < cur) {
            deadline = Some(x);
        }
    };
    d.each_view(&mut |w, v| {
        if let Some(x) = v.ripple_deadline(now) {
            fold(x);
        }
        // A told point's flash is read off the FRONT session's slot only: a
        // background tab's flash has no row to return from.
        if let Some(x) = d
            .front_session(w)
            .and_then(|s| d.table().slot(s))
            .and_then(|slot| slot.told_deadline(now))
        {
            fold(x);
        }
        // An occluded, minimized or headless window's `since` text is read by
        // nobody, so it arms nothing (the idle law's hidden-window rule).
        if v.words.is_some() && d.band_visible(w) {
            fold(v.words_due.map_or(now, |x| x.max(now)));
        }
        // A held fold, or a birth a lifted hold left owed, wakes the loop
        // whether or not the band is on screen: it is the window's geometry,
        // not text anyone reads.
        if let Some(x) = rows_deadline(d, w, v, now) {
            fold(x);
        }
    });
    // Neither a lease's lapse nor a supervisor's posts a wake of its own.
    if let Some(x) = d.table().deadline(now) {
        fold(x);
    }
    if let Some(x) = d.herald_deadline(now) {
        fold(x);
    }
    deadline
}

/// The timer's tick at `now`: a lapsed supervisor claim is removed and its
/// session re-read, every owed notification the rate limit allows is
/// posted, a lapsed cooperative lease is re-read, a finished ripple retires,
/// and every row whose `since` figure moved is recomposed (its session's
/// facts re-read first, so a fact whose change posted nothing reaches the
/// row at the tick), and a row whose held fold is due folds, or whose birth
/// a lifted hold left owed is born — after the lapses above, so a lease that
/// lapsed at this tick is what it judges.
/// Returns the windows that need a repaint.
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
        refresh_session(d, session, false);
    }
    d.herald_owed(now);
    for session in d.table().lapsed_leases(now) {
        if !d.holds_session(session) {
            // Dropped rather than re-armed every tick.
            d.forget(session);
            continue;
        }
        refresh_session(d, session, false);
        if let Some(slot) = d.table_mut().slots.get_mut(&session)
            && slot.lease_until.is_some_and(|x| x <= now)
        {
            slot.lease_until = None;
        }
    }
    for w in windows(d) {
        let ripple_done = d
            .view_of(w)
            .is_some_and(|v| v.ripple_at.is_some() && v.ripple_step(now).is_none());
        if ripple_done && let Some(v) = d.view_of_mut(w) {
            v.ripple_at = None;
            v.ripple_chose = false;
            out.push(w);
        }
        // The words' own clock: recomposed only when their figure moved,
        // never at whatever rate another owner wakes the loop.
        let row_due = d
            .view_of(w)
            .is_some_and(|v| v.words.is_some() && v.words_due.is_none_or(|x| x <= now));
        if row_due {
            let before = d.view_of(w).map(|v| v.seed);
            if let Some(session) = d.front_session(w) {
                refresh_session(d, session, false);
            }
            refresh_window(d, w);
            if let Some(v) = d.view_of_mut(w) {
                v.words_due = v.words.as_ref().map(|x| now + words_step(x));
            }
            if d.view_of(w).map(|v| v.seed) != before && !out.contains(&w) {
                out.push(w);
            }
        }
        // The held fold, or the owed birth: its session re-read first (a
        // want that came back, or went, without a wake is judged here), then
        // judged on the tick's clock.
        let geometry_due = d
            .view_of(w)
            .and_then(|v| rows_deadline(d, w, v, now))
            .is_some_and(|x| x <= now);
        if geometry_due {
            let before = d.view_of(w).map(|v| v.rows);
            if let Some(session) = d.front_session(w) {
                refresh_session(d, session, false);
            }
            refresh_window_at(d, w, now, false);
            if d.view_of(w).map(|v| v.rows) != before && !out.contains(&w) {
                out.push(w);
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// THE WIRE STRINGS (design ruling 358): what the `status` and `chrome` verbs
// print of presence, composed here so every host prints the same bytes. The
// percent codec stays the host's ([`Host::pct_encode`], through
// [`Hand::wire`](super::Hand::wire)); no second encoder can drift.

/// The highest story watermark any window holds for `session` (0: no window
/// has read its story).
fn read_mark<D: Desk>(d: &D, session: u64) -> u64 {
    let mut mark = 0;
    d.each_view(&mut |_, v| mark = mark.max(v.watermark(session)));
    mark
}

/// A session's level as `status level=` reports it: read against the
/// HIGHEST watermark any window holds for the session — a story a human
/// read in any window (the fold law moved that window's mark) is read,
/// whichever tab is in front now. The tab chip reads its own window's mark,
/// so on a one-window desktop the two never disagree: reading the FOCUSED
/// window's mark instead answered `level=story` for a story the human had
/// already folded, the moment another tab came to the front (round 19's
/// review, `adv9`). [`Level::Quiet`] for a session no refresh has touched.
#[must_use]
pub fn session_level<D: Desk>(d: &D, session: u64) -> Level {
    d.table()
        .slot(session)
        .map_or(Level::Quiet, |slot| slot.level(read_mark(d, session)))
}

/// The additive presence `status` fields: round 19's `hand=<token>
/// level=<level> story=<n>` (design §5), then `why=` — what put the session
/// at `level=attention` ([`Slot::why`]). Plain reads of the slot the
/// refreshes keep current — no lock, no classification — and `hand=-
/// level=quiet story=0 why=-` for a session no refresh has touched yet.
#[must_use]
pub fn status_tail<D: Desk>(d: &D, session: u64) -> String {
    let level = session_level(d, session).wire();
    match d.table().slot(session) {
        Some(slot) => format!(
            "hand={} level={level} story={} why={}",
            slot.hand.wire::<D::V>(),
            slot.story_seq,
            slot.why(read_mark(d, session))
        ),
        None => format!("hand=- level={level} story=0 why=-"),
    }
}

/// The row's words as the `chrome` verb reads them: the line as the PAINTER
/// fits it at `cols` terminal columns (the row keeps one margin cell each
/// side, [`text_cols`], so the wire never reports a slot the human cannot
/// see) — empty with no row — and the rim's name.
#[must_use]
pub fn report(v: &View, cols: usize) -> (String, &'static str) {
    let line = v
        .words
        .as_ref()
        .map(|w| w.fit(text_cols(cols)))
        .unwrap_or_default();
    (line, v.rim.wire())
}

/// `chrome`'s presence line for the host's front window `front` (`cols`:
/// its terminal columns) — what the human sees, as words: `presence
/// rim=<none|drive|wait|stop|stop-hold> level=<level> band="<the row, fitted
/// to the window>" sentence="<the a11y sentence>"`. The two quoted values are
/// cell-sanitized already (no control bytes); `"` and `\` are escaped so the
/// line stays one parseable record. A quiet window, or no window, prints both
/// empty; a front the host holds no view for prints an empty `rim=`, as the
/// host's own line always did (ruling 358). A row the facts want that the
/// window has not committed prints empty, with ` held=<driver|handoff|height>`
/// after the sentence: why it is not on the glass yet (ruling 365). No
/// command text, mail body, OSC title or limit message can appear here: the
/// row never carries one.
#[must_use]
pub fn chrome_line<D: Desk>(d: &D, front: Option<D::Win>, cols: usize) -> String {
    let Some(w) = front else {
        return format!(
            "presence rim=none level=quiet band={} sentence={}",
            quoted(""),
            quoted("")
        );
    };
    let view = d.view_of(w);
    // WHAT IS DRAWN, never what is projected (ruling 365; day nine): a row
    // the facts want but the window has not committed — a driver's hand on
    // the session, a handoff's freeze, a window one row tall — is not on the
    // glass, so the line prints it empty and says why it waits (`held=`).
    let drawn = view.filter(|v| v.rows > 0);
    let (band, _) = drawn.map(|v| report(v, cols)).unwrap_or_default();
    let rim = view.map(|v| v.rim.wire()).unwrap_or_default();
    let level = view.map_or(Level::Quiet, |v| v.level).wire();
    let sentence = drawn
        .and_then(|v| v.words.as_ref())
        .map_or("", |w| w.sentence.as_str());
    let held = match view {
        Some(v) if v.rows == 0 && v.words.is_some() => {
            if d.rows_frozen() {
                " held=handoff"
            } else if d.driver_may_type(w) {
                " held=driver"
            } else {
                " held=height"
            }
        }
        _ => "",
    };
    format!(
        "presence rim={rim} level={level} band={} sentence={}{held}",
        quoted(&band),
        quoted(sentence)
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
