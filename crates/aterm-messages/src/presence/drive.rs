// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The presence driver: the sequencing every host shares, as `crate::drive`
//! is for the band. It folds a session's facts into its slot and re-projects
//! every window showing it, commits the row to a window's geometry, decides
//! the fold law, starts the ripple and the choice pulse, takes a told story,
//! and computes the one deadline the host's timer arms ([`deadline`]) and the
//! tick that timer runs.
//!
//! Nothing here reads a clock, owns a timer or knows a window system: what
//! only a host can do comes in through [`Desk`], and the per-session state is
//! [`Table`]. A quiet desktop arms no deadline.

use std::collections::HashMap;

use super::{
    FOLD_LOOK_CAP, FOLD_QUIET, Facts, Host, LeaseMark, Level, Rim, Slot, StoryVerb, View, Words,
    text_cols, words, words_step,
};
use crate::Instant;
use crate::chrome;

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
    /// has one. Read at every refresh and by every [`deadline`] (once per
    /// supervised session): one leaf read, no lock held across it.
    fn supervisor_lapse(&self, session: u64, now: Instant) -> Option<Instant>;
    /// Whether a window's band is on screen (not occluded, minimized or
    /// headless): the row's `since` text arms a deadline only then.
    fn band_visible(&self, w: Self::Win) -> bool;
    /// Whether a window holds the OS key focus.
    fn key_window(&self, w: Self::Win) -> bool;
    /// Whether the motion policies let a ripple start on `w` (`focused`:
    /// whether it counts as focused for them).
    fn ripple_allowed(&self, w: Self::Win, focused: bool) -> bool;
    /// The View menu's presence band toggle.
    fn band_toggle(&self) -> bool;
    /// The View menu's presence rim toggle.
    fn rim_toggle(&self) -> bool;
    /// A seamless handoff freezes every window's row count.
    fn rows_frozen(&self) -> bool;
    /// Whether a driver may still type into any session `w` shows (a `turn`
    /// before its settle, a live drive lease). The window's row count holds
    /// meanwhile: a re-grid resizes every pane, the program repaints on the
    /// `SIGWINCH`, and a driver's `if-gen=` fence would read its own lease's
    /// re-grid as a change. The row catches up on the wake that lifts it
    /// ([`Facts::typing`]).
    fn driver_may_type(&self, w: Self::Win) -> bool {
        let _ = w;
        false
    }
    /// The live lease of `session`, as [`Facts::lease`] reduces it, read now
    /// rather than the slot's copy. The row's commit holds while it differs
    /// from the mark the front session's words were formed under
    /// (`commit_rows`), and a window refreshed meanwhile re-senses the session
    /// first. `None`: the host keeps no lease, and only [`Facts::typing`] and
    /// [`Self::driver_may_type`] hold.
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
    /// The window's chrome regrow floor ([`chrome::RegrowFloor::floor`]): the
    /// earliest instant an unforced fold may re-grid `w`,
    /// [`chrome::CHROME_REGROW_SETTLE`] past the last rise of any of its
    /// chrome rows (this row's birth, a band row, the strip). `None`: no rise
    /// noted, or a window whose grid never re-grids for its chrome.
    fn chrome_floor(&self, w: Self::Win) -> Option<Instant> {
        let _ = w;
        None
    }
    /// The row was just born on `w` at `now` (its grid is about to shrink by
    /// the one row [`Self::regrid`] pays): note the rise on `w`'s floor
    /// ([`chrome::RegrowFloor::rose`]), so no other source's fall re-grids
    /// the window back inside the settle.
    fn chrome_rose(&mut self, w: Self::Win, now: Instant) {
        let _ = (w, now);
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

/// Start the choice pulse on `w`: the same 300 ms edge flash, painted in the
/// story tone and shown even on a window with no rim. It is a rim flash, so
/// the rim toggle off means none; it is not held to the OS key focus, since
/// its point is to show a question answered in a window the person is not
/// in. Only reduced motion and serious mode stop it.
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

/// Project `w`'s front session onto its view (level, rim, words and the row's
/// existence) and commit the row count to its geometry. Asks for a repaint
/// when anything the painter reads moved or the row folded.
///
/// A front session whose lease moved since its slot was sensed
/// ([`lease_moved`]) is re-sensed first through [`refresh_session`], so no
/// refresh (a tick, a focus change, a tab switch) projects words from a gone
/// lease. Once: a lease that moves again under that read is held by
/// `commit_rows` until its own wake.
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
/// ([`View::fold_read`]).
///
/// A switch of the window's front session is not a read: a quick switch away
/// and back (two tab keys, a pane focus flip) would fold and re-bear the row,
/// re-gridding every pane twice. The switch starts the ordinary fold quiet,
/// which the switch back cancels; the person's first key in the new tab is
/// the read ([`human_acted`]).
fn refresh_window_at<D: Desk>(d: &mut D, w: D::Win, now: Instant, read: bool) {
    let session = d.front_session(w);
    let watermark = session
        .and_then(|s| d.view_of(w).map(|v| v.watermark(s)))
        .unwrap_or(0);
    let slot = session.and_then(|s| d.table().slot(s));
    let (level, rim, words) = project(slot, watermark, d.band_toggle(), d.rim_toggle(), now);
    d.projecting(w);
    let moved = d
        .view_of_mut(w)
        .is_some_and(|v| v.show(level, rim, words, now));
    due_by_told_end(d, w, now);
    let folded = commit_rows(d, w, now, read);
    if moved || folded {
        d.request_redraw(w);
    }
}

/// While the front session's told point stands in the phase slot
/// ([`super::TOLD_FLASH`]), make the row's words due again when it ends, so
/// the tick [`deadline`] names for it re-projects the phase rather than
/// leaving the flash until the words' minute clock.
fn due_by_told_end<D: Desk>(d: &mut D, w: D::Win, now: Instant) {
    let end = d
        .front_session(w)
        .and_then(|s| d.table().slot(s))
        .and_then(|slot| slot.told_deadline(now));
    if let Some(end) = end
        && let Some(v) = d.view_of_mut(w)
        && v.words.is_some()
    {
        v.words_due = Some(v.words_due.map_or(end, |due| due.min(end)));
    }
}

/// Every window: the tab-switch / focus-change / restore funnel.
pub fn refresh_all<D: Desk>(d: &mut D) {
    for w in windows(d) {
        refresh_window(d, w);
    }
}

/// Commit the row's existence to `w`'s geometry: `true` when the count moved
/// (one PTY re-grid). Frozen mid-handoff, held while a driver may type
/// ([`Desk::driver_may_type`]), and yielding to the last terminal row of a
/// real window.
///
/// The hold is judged on the words' own snapshot too: a lease let go between
/// the sense and this commit would bear a row for a gone hand. So the row
/// also holds while the words' mark ([`Slot::lease`]) differs from the live
/// one ([`Desk::lease_mark`]) or they said a driver may type
/// ([`Slot::typing`]); the wake that moved the lease re-derives the want.
/// Such a want decides nothing: no birth, no fold, no start or cancel of the
/// quiet, and [`rows_deadline`] arms no instant meanwhile. `DriverGeometry`'s
/// `NoBirthForAGoneHand` and `TheHeldRowCatchesUp` (`aterm-spec`) are the two
/// halves.
///
/// A birth commits at once (noted as a rise, [`Desk::chrome_rose`]); a fold
/// only when [`fold_due`] says and past [`Desk::chrome_floor`], so another
/// chrome row's rise just before it cannot make a shrink-then-grow pair. A
/// want that returns first cancels the fold with no re-grid. The quiet runs
/// through a hold, so a fold whose want stayed away commits as the hold
/// lifts. Two folds wait for neither: a real window that can no longer afford
/// the row, and hiding the band. `read` marks the fold a person's read
/// ([`View::fold_read`]).
fn commit_rows<D: Desk>(d: &mut D, w: D::Win, now: Instant, read: bool) -> bool {
    let front = d.front_session(w);
    let moved = front.is_some_and(|s| lease_moved(d, s));
    let typing = front.is_some_and(|s| d.table().slot(s).is_some_and(|slot| slot.typing));
    let held = d.rows_frozen() || typing || d.driver_may_type(w);
    let band = d.band_toggle();
    let geometry = d.grid_rows(w);
    let looked = d.looked_at(w);
    let floor = d.chrome_floor(w);
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
        let due = fold_due(v, looked).is_some_and(|x| now >= x) && chrome::fall_allowed(floor, now);
        if !(due || !band || window_took_it) {
            return false;
        }
        v.fold_since = None;
        v.fold_read = false;
    }
    let rose = want > v.rows;
    v.rows = want;
    if rose {
        d.chrome_rose(w, now);
    }
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
    d.table()
        .slot(session)
        .is_some_and(|slot| d.lease_mark(session).is_some_and(|live| slot.lease != live))
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
/// * A person's read ([`View::fold_read`]) folds once the row has stood
///   [`FOLD_QUIET`] since its birth: a key that folds a row milliseconds
///   after a told story bore it is a flap.
/// * Any other fold waits [`FOLD_QUIET`] past the want's drop and past the
///   last look a driver took at the window's screen ([`Desk::looked_at`]),
///   bounded by [`FOLD_LOOK_CAP`] past the drop.
///
/// The commit and `rows_deadline` add the chrome regrow floor
/// ([`Desk::chrome_floor`]) on top.
#[must_use]
pub fn fold_due(v: &View, looked: Option<Instant>) -> Option<Instant> {
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
/// * A held fold is due at [`fold_due`], and not before the window's chrome
///   regrow floor ([`chrome::at_floor`]): the commit declines until then, so
///   an earlier instant would spin the tick.
/// * A birth the words want that no hold stands against is owed `now`. A
///   hold turns the birth's projection away, and a hold that lifts without a
///   wake whose facts moved strands it (a background tab's lease taken and
///   let go before either of its wakes runs, so [`refresh_session`] projects
///   nothing); the band's words clock would catch it only a minute later, and
///   never on a headless or occluded window, so the tick commits it. No spin:
///   this excludes exactly [`commit_rows`]' holds, and the tick re-projects
///   the window before it commits.
fn rows_deadline<D: Desk>(d: &D, w: D::Win, v: &View, now: Instant) -> Option<Instant> {
    if d.rows_frozen() || d.driver_may_type(w) || words_stale(d, w) {
        return None;
    }
    if d.grid_rows(w).is_some_and(|g| row_want(v, g) > v.rows) {
        return Some(now);
    }
    Some(chrome::at_floor(fold_due(v, d.looked_at(w))?, d.chrome_floor(w)).max(now))
}

/// The fold law. The human acted (a key, a click) in `w`: if its front
/// session is calm, the story is read (the watermark moves to the newest
/// point) and the row folds. Never on focus alone, never on a timer, and
/// never while anything is still happening.
pub fn human_acted<D: Desk>(d: &mut D, w: D::Win) {
    let Some(session) = d.front_session(w) else {
        return;
    };
    human_acted_on(d, w, session);
}

/// The fold law for an act that may move the front: the human acted in `w`
/// while `session` was its front, the story they saw. A click presses the
/// layout the person saw first, and that press can bring another session
/// front (a click in another split pane) or none (a capsule that opens
/// Settings); the fold reads the session front before it. Otherwise as
/// [`human_acted`]: only while calm, once.
pub fn human_acted_on<D: Desk>(d: &mut D, w: D::Win, session: u64) {
    // Calm is judged on the live lease: a lease whose wake has not run yet
    // leaves the slot reading calm. Re-sensing a moved lease first, as
    // [`refresh_window`] does, makes both orders one: a lease just taken is
    // not calm, and one just let go is judged as let go.
    if lease_moved(d, session) {
        refresh_session(d, session, false);
    }
    let Some(seq) = d
        .table()
        .slot(session)
        .filter(|slot| slot.calm())
        .map(|slot| slot.story_seq)
    else {
        // A fold the front no longer wants is not a story read. After a switch
        // to a tab whose session wants no row, that session may not be calm
        // (attention told by the band, aterm's own hand) or may hold no slot
        // yet; calm guards reading a story away, but there is none on the row,
        // so the key folds the blank row as a read and the watermark stays.
        // Only while `session` is still the front: a click reads the session
        // front before its press, so a click from a non-calm A into a quiet B
        // would otherwise fold B's blank row and the click back re-bear it.
        let blank_pending = d.front_session(w) == Some(session)
            && d.view_of(w)
                .is_some_and(|v| v.fold_since.is_some() && !v.fold_read && v.words.is_none());
        if blank_pending {
            let now = d.clock();
            refresh_window_at(d, w, now, true);
        }
        return;
    };
    let Some(v) = d.view_of_mut(w) else {
        return;
    };
    // A row already blank whose fold is pending is still the keystroke's to
    // fold, or the blank row stands out its quiet under the person's eyes.
    if v.watermark(session) == seq && v.words.is_none() && (v.fold_since.is_none() || v.fold_read) {
        return;
    }
    v.watermarks.insert(session, seq);
    // The keystroke is a read, and the read folds the row once it has stood
    // its minimum life ([`fold_due`]); until then the timer folds it.
    let now = d.clock();
    refresh_window_at(d, w, now, true);
}

/// A told story point landed for `session` (`aterm ctl story`): the slot is
/// brought current first (a story on a session no wake has touched must not
/// print a blank row), the point is noted, and every window showing the
/// session is re-projected. A [`StoryVerb::Chose`] pulses every window whose
/// front tab shows the session (a background tab has its story dot, and a
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
/// catches up at once), a row's held fold ([`FOLD_QUIET`], not before the
/// window's chrome regrow floor) or owed birth (not while its count is held),
/// a cooperative lease's or a supervisor claim's lapse, and the host's owed
/// notification. `None` on a quiet desktop.
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
        // A told point's flash is read off the front session's slot only: a
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
/// lapsed cooperative lease is re-read, a finished ripple retires, every row
/// whose `since` figure moved or whose told flash ended is recomposed (its
/// session's facts re-read first, so a fact whose change posted nothing
/// reaches the row), and a due held fold or owed birth commits, after the
/// lapses above so a lease that lapsed at this tick is what it judges.
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
            due_by_told_end(d, w, now);
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

/// The row's words as the `chrome` verb reads them: the line as the painter
/// fits it at `cols` terminal columns (inside the margins, [`text_cols`], so
/// the wire never reports a slot the human cannot see), empty with no row,
/// and the rim's name.
#[must_use]
pub fn report<V: Host>(v: &View, cols: usize) -> (String, &'static str) {
    let line = v
        .words
        .as_ref()
        .map(|w| w.fit::<V>(text_cols(cols)))
        .unwrap_or_default();
    (line, v.rim.wire())
}

/// `chrome`'s presence line for the host's front window `front` (`cols`: its
/// terminal columns), what the human sees as words: `presence
/// rim=<none|drive|wait|stop|stop-hold> level=<level> band="<the row, fitted
/// to the window>" sentence="<the a11y sentence>"`. The quoted values are
/// cell-sanitized already; `"` and `\` are escaped so the line stays one
/// parseable record. A quiet window, or no window, prints both empty; a front
/// with no view prints an empty `rim=`. A row the facts want that the window
/// has not committed prints empty, with ` held=<driver|handoff|height>` after
/// the sentence.
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
    // What is drawn, never what is projected: a row the facts want but the
    // window has not committed (a driver's hand, a handoff's freeze, a window
    // one row tall) prints empty, and `held=` says why it waits.
    let drawn = view.filter(|v| v.rows > 0);
    let (band, _) = drawn.map(|v| report::<D::V>(v, cols)).unwrap_or_default();
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
