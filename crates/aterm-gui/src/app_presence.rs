// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The App's side of PRESENCE ([`crate::presence`]): gathering each session's
//! facts from its own leaf locks, folding them into the per-session
//! [`Slot`], projecting the FOCUSED session of every window into that window's
//! [`WindowView`] (rim, band, chip, a11y sentence), and paying the one PTY
//! resize a band row costs when it appears or folds.
//!
//! Every entry point here runs at CHANGE rate: a `Wake::LeaseChanged` /
//! `Wake::FabricChanged` / `Wake::MetaChanged`, a status-revision move from the
//! sweep, a tab switch, the human's own keystroke (the fold law), and the
//! band's once-a-second / once-a-minute `since` tick while a row is up. The
//! frame path reads plain fields on the view and allocates nothing
//! (`presence_overlay`, `presence_fp`).

use std::time::{Duration, Instant};

use aterm_core::terminal::RenderCell;
use aterm_render::Theme;
use aterm_session::SessionId;

use crate::app_render::OverlayGlow;
use crate::presence::{
    self, ChipLevel, Facts, Hand, HoldFact, Level, Link, Rim, Slot, StoryVerb, TurnFact, WindowView,
};
use crate::{App, WindowId};

/// The rim's inset-border alpha: the drop target's crisp frame, so a driven
/// window's teal reads at the same weight the drag highlight always has.
const RIM_BORDER_ALPHA: u8 = 235;
/// The wash under a HOLD (12/255, design §1): faint, readable through.
const HOLD_WASH_ALPHA: u8 = 12;
/// The ripple's peak wash, decaying to 0 over its nine steps.
const RIPPLE_WASH_PEAK: u32 = 24;

/// The per-session slots, keyed by the pool's local id.
#[derive(Default, Debug)]
pub(crate) struct PresenceTable {
    slots: std::collections::HashMap<u64, Slot>,
}

impl PresenceTable {
    pub(crate) fn slot(&self, session: u64) -> Option<&Slot> {
        self.slots.get(&session)
    }

    pub(crate) fn retire(&mut self, session: u64) {
        self.slots.remove(&session);
    }
}

#[cfg(test)]
thread_local! {
    static PTY_RESIZES_FOR_PRESENCE: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// How many window re-grids the presence row has paid for ON THIS THREAD
/// (test-only: the fold-law test's "one PTY resize per transition"; a
/// headless App is driven on its test's own thread).
#[cfg(test)]
pub(crate) fn presence_regrids() -> u64 {
    PTY_RESIZES_FOR_PRESENCE.with(|c| c.get())
}

impl App {
    /// The registry's local id for a fabric `sid`, if the session is live here.
    pub(crate) fn local_session_of(&self, sid: &SessionId) -> Option<u64> {
        self.store
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .by_sid(sid)
            .map(|h| h.local_id)
    }

    /// Gather one session's facts. Leaf locks only, each held for a copy; the
    /// terminal is TRY-locked and only when the status revision moved (the
    /// classifier gate) — contention skips the reading and the next refresh
    /// retries, the sweep's own discipline.
    fn presence_facts(&self, session: u64) -> Option<Facts> {
        let s = self.pool.get(session)?;
        let ctx = &s.ctx;
        let now_us = crate::metrics::now_us();
        let now = Instant::now();
        let (hand, lease_until) = {
            let lease = ctx.turn_lease.lock().unwrap_or_else(|p| p.into_inner());
            match lease.as_ref() {
                Some(crate::Lease::Turn { id, driver }) => (
                    Hand::DrivenTurn {
                        id: *id,
                        holder: driver.as_ref().map(|d| self.name_of_sid(d)),
                    },
                    None,
                ),
                Some(crate::Lease::Drive { holder, expires_us }) if *expires_us > now_us => (
                    Hand::DrivenLease {
                        holder: holder.clone(),
                    },
                    // The lapse posts no wake: it is a deadline instead.
                    Some(now + Duration::from_micros(expires_us - now_us)),
                ),
                _ => (Hand::None, None),
            }
        };
        // Driving ANOTHER session: an open turn on a peer whose lease names
        // this session as its driver (the same record the peer's own `◂`
        // reads, so the two bands describe one turn).
        let hand = if matches!(hand, Hand::None) {
            self.driving_target(&ctx.self_id)
                .map_or(Hand::None, |dst| Hand::Driving {
                    sid: presence::short_sid(dst.as_str()),
                })
        } else {
            hand
        };
        let hold = ctx.fabric.hold().map(|h| HoldFact {
            reason: h.reason,
            fleet: h.origin == "fleet",
        });
        let mail = ctx.fabric.mail_facts();
        let (role, attention) = {
            let meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
            (meta.role.clone(), meta.attention.clone())
        };
        let link = match crate::fabric::fabric_state() {
            "connected" => Link::Connected {
                rtt_ms: crate::fabric::fabric_link_facts().1,
            },
            // The STALL's age, not the last ack's (`fabric_link_age_ms=`): a
            // link that was quiet for an hour is not an hour stalled when it
            // drops, and a bridge still dialing has no stall to date.
            "stalled" => Link::Stalled {
                age_ms: crate::fabric::fabric_stalled_ms(),
            },
            "disconnected" => Link::Disconnected,
            _ => Link::Absent,
        };
        let turn = {
            let turns = ctx.turns.lock().unwrap_or_else(|p| p.into_inner());
            turns.high_id().and_then(|id| {
                turns
                    .since(Some(id.saturating_sub(1)))
                    .last()
                    .map(|r| TurnFact {
                        id: r.id,
                        settled: r.status == "settled",
                        dur_ms: r.dur_ms,
                        carried: r.carried,
                    })
            })
        };
        let shell = self
            .session_status
            .status(session)
            .map(|st| (st.phase.as_str(), st.since));
        let revision = self.session_status.revision(session);
        let seen = self
            .presence
            .slots
            .get(&session)
            .and_then(|s| s.revision_seen);
        // THE CLASSIFIER GATE: only when the status revision moved past the one
        // the slot was classified at (or never was), and only under a try-lock
        // the reader is not holding.
        let agent = if seen != Some(revision) {
            crate::term_try_lock(&s.term).map(|t| {
                let rows = usize::from(t.rows());
                let from = rows.saturating_sub(presence::CLASSIFY_ROWS);
                let screen: Vec<String> = (from..rows)
                    .map(|r| crate::control::visible_row(&t, r))
                    .collect();
                presence::classify(&screen, now)
            })
        } else {
            None
        };
        Some(Facts {
            role,
            attention,
            shell,
            revision,
            agent,
            hand,
            lease_until,
            hold,
            mail,
            link,
            turn,
        })
    }

    /// The name the band gives a driving session: its `meta role` when it is
    /// a local session with one, else its short sid. WHO drives is the
    /// lease's own record ([`crate::Lease::Turn::driver`] — the source of the
    /// edge the turn came over, stamped when the turn took the lease), never
    /// inferred from the edge table: a standing write edge is authority, not
    /// a hand, and an Owner-token turn (the CLI, a human at another
    /// instance's keyboard) has no driver to name however many edges stand.
    fn name_of_sid(&self, sid: &SessionId) -> String {
        let role = self
            .store
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .by_sid(sid)
            .and_then(|h| {
                h.ctx
                    .meta
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .role
                    .clone()
            });
        role.unwrap_or_else(|| presence::short_sid(sid.as_str()))
    }

    /// The session `src` is driving RIGHT NOW: one whose open turn lease names
    /// `src` as its driver. A standing edge is not a hand on the keyboard
    /// (between turns a manager drives nobody, and with two workers granted
    /// the first in the registry was named for both), so only a lease counts.
    fn driving_target(&self, src: &SessionId) -> Option<SessionId> {
        let handles = self
            .store
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .snapshot();
        handles.iter().find_map(|h| {
            if &h.sid == src {
                return None;
            }
            let lease = h.ctx.turn_lease.lock().unwrap_or_else(|p| p.into_inner());
            matches!(
                lease.as_ref(),
                Some(crate::Lease::Turn { driver: Some(d), .. }) if d == src
            )
            .then(|| h.sid.clone())
        })
    }

    /// A fact about `session` changed: re-read it, fold it in, and project it
    /// onto every window that shows the session. `submitted` marks a turn's
    /// submit keypress — the ripple's edge.
    pub(crate) fn refresh_presence_session(&mut self, session: u64, submitted: bool) {
        let now = Instant::now();
        let Some(facts) = self.presence_facts(session) else {
            return;
        };
        let changed = {
            let slot = self
                .presence
                .slots
                .entry(session)
                .or_insert_with(|| Slot::new(now));
            let first = slot.revision_seen.is_none() && slot.shell.is_none();
            slot.absorb(facts, now) || first
        };
        if !changed && !submitted {
            return;
        }
        let mut windows = self.windows_with_focused_session(session);
        for (wid, _) in self.tabs_viewing_session(session) {
            if !windows.contains(&wid) {
                windows.push(wid);
            }
        }
        for wid in &windows {
            if self.focused_session_id(*wid) == Some(session) && submitted {
                self.start_presence_ripple(*wid, now);
            }
            self.refresh_presence_window(*wid);
        }
        // The chips: every strip showing the session re-stamps its levels.
        self.refresh_tab_chrome_windows(windows);
    }

    /// The wake arms: resolve the fabric sid to the pool's local id.
    pub(crate) fn on_presence_wake(&mut self, sid: &SessionId, submitted: bool) {
        if let Some(session) = self.local_session_of(sid) {
            self.refresh_presence_session(session, submitted);
        }
    }

    /// Start the 300 ms edge ripple on `wid` — unless motion is reduced, where
    /// the amplitude is 0 and the ripple never starts (the same image).
    pub(crate) fn start_presence_ripple(&mut self, wid: WindowId, now: Instant) {
        let focused = self.windows.get(&wid).is_some_and(|ws| ws.focused);
        let focused = self.motion_focus(wid, focused);
        let animate = self
            .motion_policy(focused)
            .animate(crate::motion::MotionEffect::PresenceRipple)
            && self
                .serious_mode_policy()
                .allows(crate::motion::SeriousEffect::PresenceRipple);
        if !animate {
            return;
        }
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.presence.ripple_at = Some(now);
            if let Some(w) = &ws.os_window {
                w.request_redraw();
            }
        }
    }

    /// Project the window's FOCUSED session onto its view: level, rim, words,
    /// and the band row's existence. Bumps the view's seed when anything the
    /// painter reads moved, and pays the re-grid when the row appears or folds.
    pub(crate) fn refresh_presence_window(&mut self, wid: WindowId) {
        let now = Instant::now();
        let session = self.focused_session_id(wid);
        let watermark = session
            .and_then(|s| self.windows.get(&wid).map(|ws| ws.presence.watermark(s)))
            .unwrap_or(0);
        let (level, words) = match session.and_then(|s| self.presence.slots.get(&s)) {
            Some(slot) => {
                let level = slot.level(watermark);
                let words = level
                    .shows_row()
                    .then(|| presence::words(slot, now, watermark));
                (level, words)
            }
            None => (Level::Quiet, None),
        };
        // THE VIEW TOGGLES (round 19, `[presence] band` / `rim`): an unchecked
        // band hides the row (no words ⇒ no row committed, and `chrome`
        // reports an empty band — what the human sees); an unchecked rim
        // paints none. The LEVEL is untouched: `status level=` and the chip
        // still say the fact, and the a11y sentence rides the band's words.
        let words = if self.presence_band_on() { words } else { None };
        let rim = if self.presence_rim_on() {
            level.rim()
        } else {
            presence::Rim::None
        };
        // The front window's hold, for the native menu's synchronous validate
        // (the Fabric menu's halt pair reads it when the menu opens).
        if self.frontmost_window == Some(wid) {
            self.publish_front_hold(wid);
        }
        let mut moved = false;
        if let Some(ws) = self.windows.get_mut(&wid) {
            let v = &mut ws.presence;
            if v.level != level || v.rim != rim || v.words != words {
                v.level = level;
                v.rim = rim;
                v.words = words;
                v.seed = v.seed.wrapping_add(1).max(1);
                moved = true;
            }
        }
        let folded = self.sync_presence_rows(wid);
        if (moved || folded)
            && let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref())
        {
            w.request_redraw();
        }
    }

    /// Every window: the tab-switch / focus-change / restore funnel.
    pub(crate) fn refresh_presence_all_windows(&mut self) {
        let wids: Vec<WindowId> = self.windows.keys().copied().collect();
        for wid in wids {
            self.refresh_presence_window(wid);
        }
    }

    /// Commit the band row's existence to the window geometry: `true` when the
    /// count moved (and the window was re-gridded — ONE PTY resize). Frozen
    /// mid-handoff for the same reason `sync_status_bar_rows` is, and yielding
    /// to the last terminal row the same way.
    pub(crate) fn sync_presence_rows(&mut self, wid: WindowId) -> bool {
        if self.pending_update_handoff.is_some() || self.incoming_handoff_pending {
            return false;
        }
        let Some(ws) = self.windows.get(&wid) else {
            return false;
        };
        let want = u16::from(ws.presence.words.is_some());
        let afford = ws
            .rows
            .saturating_add(ws.presence.rows)
            .saturating_sub(1)
            .min(1);
        let want = if ws.os_window.is_some() {
            want.min(afford)
        } else {
            want
        };
        if want == ws.presence.rows {
            return false;
        }
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.presence.rows = want;
            ws.presence.cached_key = None;
        }
        #[cfg(test)]
        PTY_RESIZES_FOR_PRESENCE.with(|c| c.set(c.get() + 1));
        self.regrid_window_for_chrome_rows(wid);
        true
    }

    /// THE FOLD LAW. The human acted (a key, a click) in `wid`: if its session
    /// is CALM, the story is read — the watermark moves to the newest point
    /// and the row folds (one PTY resize). Never on focus alone, never on a
    /// timer, and never while anything is still happening.
    pub(crate) fn note_human_acted(&mut self, wid: WindowId) {
        let Some(session) = self.focused_session_id(wid) else {
            return;
        };
        let Some(slot) = self.presence.slots.get(&session) else {
            return;
        };
        if !slot.calm() {
            return;
        }
        let seq = slot.story_seq;
        let Some(ws) = self.windows.get_mut(&wid) else {
            return;
        };
        if ws.presence.watermark(session) == seq && ws.presence.words.is_none() {
            return;
        }
        ws.presence.watermarks.insert(session, seq);
        self.refresh_presence_window(wid);
    }

    /// The band's `since` text ticks — once a second while it shows seconds,
    /// once a minute after — and the ripple steps. `None` on a quiet desktop.
    pub(crate) fn presence_deadline(&self, now: Instant) -> Option<Instant> {
        let mut deadline: Option<Instant> = None;
        let mut fold = |d: Instant| {
            if deadline.is_none_or(|cur| d < cur) {
                deadline = Some(d);
            }
        };
        for (wid, ws) in &self.windows {
            let v = &ws.presence;
            if let Some(d) = v.ripple_deadline(now) {
                fold(d);
            }
            // A told point's three seconds in the phase slot: the one deadline
            // a `ctl story` adds. Read off the focused session's slot only —
            // a background tab's flash has no row to return from.
            if let Some(d) = self
                .focused_session_id(*wid)
                .and_then(|s| self.presence.slot(s))
                .and_then(|slot| slot.told_deadline(now))
            {
                fold(d);
            }
            if let Some(words) = &v.words {
                // A clause that prints SECONDS anywhere (`12s`, `3m12s`,
                // `since 3m12s`, `held 1m00s, resumed 40s ago`) moves every
                // second; one that prints only minutes, hours or days moves
                // once a minute.
                let fine = words.since.iter().any(|c| prints_seconds(c));
                fold(
                    now + if fine {
                        Duration::from_secs(1)
                    } else {
                        Duration::from_secs(60)
                    },
                );
            }
        }
        // A cooperative lease's lapse: nothing posts for it, so it is a
        // deadline — the hand is re-read when it passes (`presence_tick`).
        for slot in self.presence.slots.values() {
            if let Some(d) = slot.lease_until {
                fold(d.max(now));
            }
        }
        deadline
    }

    /// The timer's tick: recompose the words of every window with a row up (the
    /// `since` figures moved) and retire a finished ripple. Returns the windows
    /// that need a repaint.
    pub(crate) fn presence_tick(&mut self, now: Instant) -> Vec<WindowId> {
        let mut out = Vec::new();
        // The facts a timer can move without a wake: a cooperative lease that
        // LAPSED (its driver crashed; nothing posts for a lapse) is re-read the
        // moment its deadline passes, on every session that holds one.
        let lapsed: Vec<u64> = self
            .presence
            .slots
            .iter()
            .filter(|(_, slot)| slot.lease_until.is_some_and(|d| d <= now))
            .map(|(session, _)| *session)
            .collect();
        for session in lapsed {
            if self.pool.get(session).is_none() {
                // A session the pool no longer holds cannot be re-read: its
                // deadline is dropped rather than re-armed every tick.
                self.presence.retire(session);
                continue;
            }
            self.refresh_presence_session(session, false);
            if let Some(slot) = self.presence.slots.get_mut(&session)
                && slot.lease_until.is_some_and(|d| d <= now)
            {
                slot.lease_until = None;
            }
        }
        let wids: Vec<WindowId> = self.windows.keys().copied().collect();
        for wid in wids {
            let ripple_done = self.windows.get(&wid).is_some_and(|ws| {
                ws.presence.ripple_at.is_some() && ws.presence.ripple_step(now).is_none()
            });
            if ripple_done && let Some(ws) = self.windows.get_mut(&wid) {
                ws.presence.ripple_at = None;
                out.push(wid);
            }
            let row_up = self
                .windows
                .get(&wid)
                .is_some_and(|ws| ws.presence.words.is_some());
            if row_up {
                let before = self.windows.get(&wid).map(|ws| ws.presence.seed);
                // The belt under the wakes' braces: while a row is up, its
                // session's facts are re-read at the tick (leaf locks, the
                // classifier still gated on the revision), so a fact whose
                // change posted nothing — the link coming back, a lease that
                // lapsed — reaches the row at the tick, never later.
                if let Some(session) = self.focused_session_id(wid) {
                    self.refresh_presence_session(session, false);
                }
                self.refresh_presence_window(wid);
                if self.windows.get(&wid).map(|ws| ws.presence.seed) != before
                    && !out.contains(&wid)
                {
                    out.push(wid);
                }
            }
        }
        out
    }

    /// The rim as an [`OverlayGlow`] for the frame path — plain field reads and
    /// arithmetic, no lock, no allocation; `None` on a quiet window.
    pub(crate) fn presence_overlay(&self, wid: WindowId, now: Instant) -> Option<OverlayGlow> {
        let ws = self.windows.get(&wid)?;
        let v = &ws.presence;
        // The quiet frame answers before the tones are derived: the four
        // contrast-floored tones cost ~1.9 µs (measured, `adv8`), and every
        // composed frame of every window took it for a `None` (round 19's
        // review, C1). A rim pays it; a quiet window pays the match.
        if matches!(v.rim, Rim::None) {
            return None;
        }
        let tones = crate::chrome_band::presence_tones(self.chrome_palette_theme());
        let (accent, mut wash_a, scale) = match v.rim {
            Rim::None => return None,
            Rim::Drive => (tones.drive, 0u8, 16u8),
            Rim::Wait => (tones.wait, 0, 16),
            Rim::Stop { hold: false } => (tones.stop, 0, 16),
            Rim::Stop { hold: true } => (tones.stop, HOLD_WASH_ALPHA, 32),
        };
        let mut border_a = RIM_BORDER_ALPHA;
        if let Some(step) = v.ripple_step(now) {
            // One edge flash, decaying over nine frames: the border to full and
            // a wash that fades out — the pixels change, the fact does not.
            border_a = 255;
            let remaining = presence::RIPPLE_STEPS.saturating_sub(step);
            wash_a = wash_a.max((RIPPLE_WASH_PEAK * remaining / presence::RIPPLE_STEPS) as u8);
        }
        Some(OverlayGlow {
            accent: pack(accent),
            wash_a,
            border_a,
            border_scale_q4: scale,
        })
    }

    /// The repaint-key term for `wid`: 0 on a quiet window.
    pub(crate) fn presence_fp(&self, wid: WindowId, now: Instant) -> u64 {
        self.windows.get(&wid).map_or(0, |ws| ws.presence.fp(now))
    }

    /// The band's painted row for this window at `cols`, cached on
    /// `(seed, cols, palette)` like the status bars' rows and BORROWED from the
    /// cache — a hit on the frame path clones nothing. Empty when no row is
    /// committed.
    pub(crate) fn presence_band_row(
        &mut self,
        wid: WindowId,
        cols: usize,
        theme: Theme,
    ) -> &[RenderCell] {
        let Some(ws) = self.windows.get(&wid) else {
            return &[];
        };
        if ws.presence.rows == 0 {
            return &[];
        }
        let palette_key = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            theme.bg.hash(&mut h);
            theme.fg.hash(&mut h);
            theme.cursor.hash(&mut h);
            h.finish()
        };
        let key = (ws.presence.seed, cols, palette_key);
        if ws.presence.cached_key != Some(key) {
            let row = match &ws.presence.words {
                Some(words) => crate::status_bars::paint_presence_row(words, cols, theme),
                None => crate::status_bars::blank_band_row(cols, theme),
            };
            if let Some(ws) = self.windows.get_mut(&wid) {
                ws.presence.cached_row = row;
                ws.presence.cached_key = Some(key);
            }
        }
        match self.windows.get(&wid) {
            Some(ws) => ws.presence.cached_row.as_slice(),
            None => &[],
        }
    }

    /// Stamp each tab's presence CHIP level onto the strip metadata (the
    /// `stamp_conn_drop_target` shape): the highest level across the tab's
    /// terminal leaves, folded over the indicator bit's own `Wait`.
    pub(crate) fn stamp_presence_chips(
        &self,
        wid: WindowId,
        metadata: &mut [crate::tab_bar::TabStripMetadata],
    ) {
        let Some(ws) = self.windows.get(&wid) else {
            return;
        };
        for (tab, item) in ws.tab_set.tabs().iter().zip(metadata.iter_mut()) {
            let mut level = item.attention;
            tab.root.any_leaf(&mut |view| {
                if let Some(session) = self
                    .view_store
                    .get(*view)
                    .copied()
                    .and_then(crate::tab_model::View::terminal_session)
                    && let Some(slot) = self.presence.slot(session)
                {
                    // Each tab's story is read against THIS window's watermark
                    // for ITS session: a story read here stays read, and every
                    // other tab shows its story until it is looked at.
                    let watermark = ws.presence.watermark(session);
                    level = level.max(slot.level(watermark).chip());
                }
                false
            });
            item.attention = level;
        }
    }

    /// The band's a11y message, when the row is committed: the sentence a
    /// screen reader speaks ("driven, turn 41, busy, …"), on bar row 0.
    #[cfg(a11y_tree)]
    pub(crate) fn presence_a11y_message(
        &self,
        wid: WindowId,
    ) -> Option<crate::accesskit_tree::GridMessage> {
        let ws = self.windows.get(&wid)?;
        if ws.presence.rows == 0 {
            return None;
        }
        let words = ws.presence.words.as_ref()?;
        Some(crate::accesskit_tree::GridMessage {
            message: crate::accesskit_tree::ChromeMessage::PresenceStatus,
            text: words.sentence.clone(),
            detail: Some(words.fit(crate::status_bars::presence_text_cols(usize::from(ws.cols)))),
            progress: None,
            activates: false,
            bar_row: Some(0),
        })
    }

    /// The band's words as the `chrome`/`status` verbs read them: the line as
    /// the PAINTER fits it (the row keeps one margin cell each side, so the
    /// wire never reports a slot the human cannot see) and the rim's name, or
    /// `None` when the window is quiet.
    pub(crate) fn presence_report(&self, wid: WindowId) -> Option<(String, &'static str)> {
        let ws = self.windows.get(&wid)?;
        let rim = ws.presence.rim.wire();
        let line = ws
            .presence
            .words
            .as_ref()
            .map(|w| w.fit(crate::status_bars::presence_text_cols(usize::from(ws.cols))))
            .unwrap_or_default();
        Some((line, rim))
    }

    /// Re-grid ONE window after its chrome rows moved (the per-window twin of
    /// `regrid_for_chrome_rows`): the strip cache and the present key are
    /// cleared, and the window is re-gridded from its own OS size — which is
    /// the one PTY resize.
    pub(crate) fn regrid_window_for_chrome_rows(&mut self, wid: WindowId) {
        let size = {
            let Some(ws) = self.windows.get_mut(&wid) else {
                return;
            };
            ws.tab_segments.clear();
            ws.last_strip_fp = None;
            ws.last_present = None;
            ws.os_window.as_ref().map(|w| w.inner_size())
        };
        if let Some(size) = size {
            self.on_resize(wid, size);
            if let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref()) {
                #[cfg(target_os = "linux")]
                w.set_min_inner_size(Some(self.whole_cell_min_size(wid)));
                w.request_redraw();
            }
        }
        // Headless (no OS window): the logical grid keeps its inserted size;
        // the caches above are the whole re-grid.
    }

    /// The window's presence level — the `chrome` line's `level=` and the
    /// tests' probe.
    pub(crate) fn presence_level(&self, wid: WindowId) -> Level {
        self.windows
            .get(&wid)
            .map_or(Level::Quiet, |ws| ws.presence.level)
    }

    /// The window's view, for the tests and the introspection verbs.
    pub(crate) fn presence_view(&self, wid: WindowId) -> Option<&WindowView> {
        self.windows.get(&wid).map(|ws| &ws.presence)
    }

    /// `chrome`'s presence line for the front window — what the human sees,
    /// as words: `presence rim=<none|drive|wait|stop|stop-hold> level=<level>
    /// band="<the row, fitted to the window>" sentence="<the a11y sentence>"`.
    /// The two quoted values are cell-sanitized already (no control bytes);
    /// `"` and `\\` are escaped so the line stays one parseable record. A
    /// quiet window prints both empty. No command text, mail body, OSC title
    /// or limit message can appear here: the band never carries one.
    pub(crate) fn presence_chrome_line(&self) -> String {
        let wid = self
            .frontmost_window
            .or_else(|| self.windows.keys().next().copied());
        let quote = |s: &str| {
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
        };
        let Some(wid) = wid else {
            return format!(
                "presence rim=none level=quiet band={} sentence={}",
                quote(""),
                quote("")
            );
        };
        let (band, rim) = self.presence_report(wid).unwrap_or_default();
        let level = self.presence_level(wid).wire();
        let sentence = self
            .presence_view(wid)
            .and_then(|v| v.words.as_ref())
            .map(|w| w.sentence.clone())
            .unwrap_or_default();
        format!(
            "presence rim={rim} level={level} band={} sentence={}",
            quote(&band),
            quote(&sentence)
        )
    }

    /// A session's level as `status level=` reports it: read against the
    /// HIGHEST watermark any window holds for the session — a story a human
    /// read in any window (the fold law moved that window's mark) is read,
    /// whichever tab is in front now. The tab chip reads its own window's
    /// mark, so on the one-window desktop the two never disagree: reading the
    /// FOCUSED window's mark instead answered `level=story` for a story the
    /// human had already folded, the moment another tab came to the front
    /// (round 19's review, `adv9`).
    pub(crate) fn presence_session_level(&self, session: u64) -> Level {
        let Some(slot) = self.presence.slot(session) else {
            return Level::Quiet;
        };
        let watermark = self
            .windows
            .values()
            .map(|ws| ws.presence.watermark(session))
            .max()
            .unwrap_or(0);
        slot.level(watermark)
    }

    /// The three additive `status` fields of round 19: `hand=<token>
    /// level=<level> story=<n>` (design §5). Read from the slot the wakes keep
    /// current — no lock, no classification — and `hand=- level=quiet story=0`
    /// for a session no wake has touched yet.
    pub(crate) fn presence_status_tail(&self, session: u64) -> String {
        let level = self.presence_session_level(session).wire();
        match self.presence.slot(session) {
            Some(slot) => format!(
                "hand={} level={level} story={}",
                slot.hand.wire(),
                slot.story_seq
            ),
            None => format!("hand=- level={level} story=0"),
        }
    }

    /// `aterm ctl story <verb> [<text>]` landed for `session`: the slot is
    /// brought current first (a story on a session no wake has touched must
    /// not print a blank row), the point is noted, and every window showing
    /// the session is re-projected — the phase slot reads the verb for
    /// [`presence::TOLD_FLASH`], then returns to the phase on the timer. The
    /// reply is the point's seq; `Err` for a session the pool no longer holds.
    pub(crate) fn tell_story(
        &mut self,
        session: u64,
        verb: StoryVerb,
        text: &str,
    ) -> Result<u64, &'static str> {
        if self.pool.get(session).is_none() {
            return Err("no such session");
        }
        self.refresh_presence_session(session, false);
        let now = Instant::now();
        let seq = self
            .presence
            .slots
            .entry(session)
            .or_insert_with(|| Slot::new(now))
            .tell(verb, text, now);
        let mut windows = self.windows_with_focused_session(session);
        for (wid, _) in self.tabs_viewing_session(session) {
            if !windows.contains(&wid) {
                windows.push(wid);
            }
        }
        for wid in &windows {
            self.refresh_presence_window(*wid);
        }
        self.refresh_tab_chrome_windows(windows);
        Ok(seq)
    }

    /// Forget a retired session's slot, and every window's watermark for it.
    pub(crate) fn retire_presence(&mut self, session: u64) {
        self.presence.retire(session);
        for ws in self.windows.values_mut() {
            ws.presence.watermarks.remove(&session);
        }
    }
}

/// Whether a `since` clause prints a seconds figure (`12s`, `3m12s`, `since
/// 40s`, `held 1m00s, resumed 5s ago`) — a token ending in `s` right after a
/// digit; `3 turns`, `2h05m` and `1d 22h` do not.
fn prints_seconds(clause: &str) -> bool {
    clause
        .split(|c: char| !c.is_ascii_alphanumeric())
        .any(|tok| {
            tok.strip_suffix('s')
                .is_some_and(|head| head.chars().last().is_some_and(|c| c.is_ascii_digit()))
        })
}

fn pack(c: [u8; 3]) -> u32 {
    (u32::from(c[0]) << 16) | (u32::from(c[1]) << 8) | u32::from(c[2])
}

/// The chip level the existing indicator bits spell: `Wait`.
pub(crate) fn chip_of_attention(attention: bool) -> ChipLevel {
    if attention {
        ChipLevel::Wait
    } else {
        ChipLevel::Off
    }
}

#[cfg(test)]
mod tests {
    //! Item 7 of the round-19 contract, the slice this commit lands: the
    //! headless model, the fold law, the idle repaint invariant, the classifier
    //! gate, the golden cells, the chip level and the a11y sentence. The rim
    //! images ride the sacred `image` path in `app_introspect`'s capture tests.

    use super::*;
    use crate::presence::{AgentPhase, AgentReading, Level, Words};
    use crate::{App, WindowId};
    use std::sync::Arc;

    fn app_with_stub() -> (App, WindowId, u64, Arc<crate::SessionCtx>) {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let sid = app.next_session_id;
        app.push_stub_tab(wid, crate::stub_session(sid));
        let ctx = app.pool.get(sid).expect("stub pooled").ctx.clone();
        assert_eq!(
            app.focused_session_id(wid),
            Some(sid),
            "the stub is the front session"
        );
        (app, wid, sid, ctx)
    }

    fn line(app: &App, wid: WindowId) -> (String, &'static str) {
        app.presence_report(wid).expect("a window")
    }

    /// THE HEADLESS MODEL TEST (§7): a turn submit puts the rim on and the hand
    /// in the band before any status revision moves; a hold is the stop rim and
    /// `⊘ hold` within one wake; a delivery prints `✉1 task←✓x` with the trust
    /// glyph BEFORE the sender; a hold lands only on the session it names — a
    /// second session in the same window stays quiet (`bridge_lost` holds only
    /// the sessions the bridge touched, and this is the read side of that).
    #[test]
    fn a_turn_a_hold_and_a_delivery_reach_the_rim_and_the_band_at_change_rate() {
        let (mut app, wid, sid, ctx) = app_with_stub();
        assert_eq!(app.presence_level(wid), Level::Quiet);
        assert_eq!(
            app.chrome_rows(wid),
            app.chrome_rows_shared(),
            "quiet: no row"
        );
        let revision = app.session_status.revision(sid);

        // A peer's turn opens: the lease is the fact, the wake carries it.
        *ctx.turn_lease.lock().unwrap() = Some(crate::Lease::Turn {
            id: 41,
            driver: None,
        });
        app.on_presence_wake(&ctx.self_id, true);
        assert_eq!(app.presence_level(wid), Level::Driven);
        let (text, rim) = line(&app, wid);
        assert_eq!(rim, "drive");
        assert!(text.contains("\u{25c2} turn 41"), "{text}");
        assert_eq!(
            app.chrome_rows(wid),
            app.chrome_rows_shared() + 1,
            "the band row"
        );
        assert_eq!(
            app.session_status.revision(sid),
            revision,
            "before the next status revision"
        );

        // A fleet hold: stop rim (2x, washed) and the hand slot says why.
        assert!(crate::fabric::apply_hold_for_test(
            &ctx,
            Some(crate::fabric::Hold {
                reason: "pause".into(),
                origin: "fleet".into(),
            })
        ));
        app.on_presence_wake(&ctx.self_id, false);
        assert_eq!(app.presence_level(wid), Level::Hold);
        let (text, rim) = line(&app, wid);
        assert_eq!(rim, "stop-hold");
        assert!(
            text.contains("\u{2298} hold pause \u{00b7}fleet\u{1f512}"),
            "{text}"
        );
        assert!(crate::fabric::apply_hold_for_test(&ctx, None));
        *ctx.turn_lease.lock().unwrap() = None;
        app.on_presence_wake(&ctx.self_id, false);
        assert_ne!(app.presence_level(wid), Level::Hold);

        // A task lands: unread, trust first, then the sender.
        let reply = crate::fabric::cmd_deliver(
            &app.store,
            &format!(
                "{} off=1 from=h-x kind=task trust=agent text=hello",
                ctx.self_id.as_str()
            ),
        );
        assert!(reply.starts_with("OK"), "{reply}");
        app.on_presence_wake(&ctx.self_id, false);
        let (text, rim) = line(&app, wid);
        assert!(text.contains("\u{2709}1 task\u{2190}\u{2713}h-x"), "{text}");
        assert_eq!(
            app.presence_level(wid),
            Level::Attention,
            "an unread task wants a human"
        );
        assert_eq!(rim, "wait");
        assert!(!text.contains("hello"), "no body on the band: {text}");

        // A second session in the window: a hold on it holds only it.
        let sid2 = app.next_session_id;
        app.push_stub_tab(wid, crate::stub_session(sid2));
        let ctx2 = app.pool.get(sid2).unwrap().ctx.clone();
        assert!(crate::fabric::apply_hold_for_test(
            &ctx2,
            Some(crate::fabric::Hold {
                reason: "bridge-lost".into(),
                origin: "fleet".into(),
            })
        ));
        app.on_presence_wake(&ctx2.self_id, false);
        assert!(app.presence.slot(sid2).unwrap().hold.is_some());
        assert!(
            app.presence.slot(sid).unwrap().hold.is_none(),
            "untouched stays unheld"
        );
        assert_eq!(app.presence.slot(sid).unwrap().level(0), Level::Attention);
    }

    /// THE FOLD LAW (§7): a keypress while the session is not calm is not a
    /// fold; calm without a keypress (any number of ticks) is not a fold; both
    /// together fold the row — and the fold is ONE PTY re-grid.
    #[test]
    fn the_row_folds_only_on_a_keypress_while_calm_and_pays_one_regrid() {
        let (mut app, wid, _sid, ctx) = app_with_stub();
        let regrids = presence_regrids();
        assert!(crate::fabric::apply_hold_for_test(
            &ctx,
            Some(crate::fabric::Hold {
                reason: "pause".into(),
                origin: "local".into(),
            })
        ));
        app.on_presence_wake(&ctx.self_id, false);
        assert_eq!(app.presence_view(wid).unwrap().rows, 1);
        assert_eq!(
            presence_regrids(),
            regrids + 1,
            "the row appearing is one re-grid"
        );

        // A key while HELD: not calm, no fold.
        app.note_human_acted(wid);
        assert_eq!(app.presence_view(wid).unwrap().rows, 1);
        assert_eq!(presence_regrids(), regrids + 1);

        // The hold lifts: the story ("held, resumed") keeps the row up.
        assert!(crate::fabric::apply_hold_for_test(&ctx, None));
        app.on_presence_wake(&ctx.self_id, false);
        assert_eq!(app.presence_level(wid), Level::Story);
        assert_eq!(app.presence_view(wid).unwrap().rows, 1);
        assert_eq!(presence_regrids(), regrids + 1, "a text edit never resizes");

        // Calm without a key: ticks fold nothing.
        let now = Instant::now();
        for s in 1..=120u64 {
            app.presence_tick(now + Duration::from_secs(s));
        }
        assert_eq!(app.presence_view(wid).unwrap().rows, 1, "never on a timer");
        assert_eq!(app.presence_level(wid), Level::Story);

        // The key, while calm: the fold, and exactly one re-grid.
        app.note_human_acted(wid);
        assert_eq!(app.presence_level(wid), Level::Quiet);
        assert_eq!(app.presence_view(wid).unwrap().rows, 0);
        assert_eq!(presence_regrids(), regrids + 2);
        assert_eq!(app.chrome_rows(wid), app.chrome_rows_shared());
        // …and a second key changes nothing.
        app.note_human_acted(wid);
        assert_eq!(presence_regrids(), regrids + 2);
    }

    /// THE REPAINT INVARIANT (§7): on a quiet window `presence_fp` is exactly 0
    /// across a thousand frames, the overlay is `None`, and the frame path
    /// touches nothing that could allocate — the painted-row cache is not
    /// even created (no capacity), the view's fields are plain data, and the
    /// two frame-path reads take `&self` and return `Copy` values.
    /// Round 19, item 5: `aterm ctl story` reaches the band within one
    /// revision — the phase slot reads `✓ approved`, `status` says `level=story
    /// story=<n>`, `chrome` prints the same words the human sees — and the
    /// slot returns to the phase after three seconds (the timer's deadline is
    /// armed; the words at `now + 3 s` are the story summary, which counts the
    /// approval). A session the pool does not hold is an error, not a row.
    #[test]
    fn a_told_story_reaches_the_band_and_the_verbs_within_one_revision() {
        use crate::presence::{StoryVerb, TOLD_FLASH};
        let (mut app, wid, sid, _ctx) = app_with_stub();
        let revision = app.session_status.revision(sid);
        let record = app.session_status_record(sid).expect("live");
        assert!(record.ends_with(" hand=- level=quiet story=0"), "{record}");
        assert_eq!(
            app.presence_chrome_line(),
            "presence rim=none level=quiet band=\"\" sentence=\"\"",
            "a quiet window prints empty quotes"
        );

        assert_eq!(app.tell_story(sid, StoryVerb::Approval, ""), Ok(1));
        let (text, rim) = line(&app, wid);
        assert!(text.contains("\u{2713} approved"), "{text}");
        assert_eq!(rim, "none", "a story has no rim");
        assert_eq!(app.presence_level(wid), Level::Story);
        let record = app.session_status_record(sid).expect("live");
        assert!(record.ends_with(" hand=- level=story story=1"), "{record}");
        assert_eq!(
            app.session_status.revision(sid),
            revision,
            "before the next status revision"
        );
        let chrome = app.presence_chrome_line();
        assert!(
            chrome.starts_with("presence rim=none level=story band=\""),
            "{chrome}"
        );
        assert!(chrome.contains("\u{2713} approved"), "{chrome}");
        assert!(
            chrome.ends_with(" sentence=\"approved by watcher\""),
            "{chrome}"
        );
        let now = Instant::now();
        let deadline = app
            .presence_deadline(now)
            .expect("the flash arms the timer");
        assert!(deadline <= now + TOLD_FLASH, "the slot returns within 3 s");

        // Three seconds on: the phase slot is the story summary, counting it.
        let slot = app.presence.slot(sid).expect("a slot");
        let later = presence::words(slot, now + TOLD_FLASH, 0);
        assert_eq!(later.phase, "\u{25c7} quiet", "{later:?}");
        assert!(later.since.iter().any(|c| c == "1 approval"), "{later:?}");

        // A verb with text: the text is the detail, sanitized, and the seq moves.
        assert_eq!(
            app.tell_story(sid, StoryVerb::Exit, "session\u{202e} gone"),
            Ok(2)
        );
        let (text, _) = line(&app, wid);
        assert!(text.contains("\u{2715} exit"), "{text}");
        assert!(text.contains("session gone"), "{text}");
        assert!(
            !text.contains('\u{202e}'),
            "a bidi override never reaches the chrome"
        );
        let record = app.session_status_record(sid).expect("live");
        assert!(record.ends_with(" level=story story=2"), "{record}");
        assert_eq!(
            app.tell_story(9999, StoryVerb::Timeout, ""),
            Err("no such session")
        );

        // The hand rides `status` too: a peer's open turn.
        *_ctx.turn_lease.lock().unwrap() = Some(crate::Lease::Turn {
            id: 41,
            driver: None,
        });
        app.on_presence_wake(&_ctx.self_id, false);
        let record = app.session_status_record(sid).expect("live");
        assert!(
            record.ends_with(" hand=turn:41 level=driven story=2"),
            "{record}"
        );
        let chrome = app.presence_chrome_line();
        assert!(
            chrome.starts_with("presence rim=drive level=driven band=\""),
            "{chrome}"
        );
    }

    #[test]
    fn presence_fp_is_zero_over_a_thousand_idle_frames() {
        let (mut app, wid, sid, _ctx) = app_with_stub();
        app.refresh_presence_session(sid, false);
        app.refresh_presence_all_windows();
        let now = Instant::now();
        let cap_before = app.presence_view(wid).unwrap().cached_row.capacity();
        for i in 0..1000u64 {
            let t = now + Duration::from_millis(i);
            assert_eq!(app.presence_fp(wid, t), 0);
            assert!(app.presence_overlay(wid, t).is_none());
            assert!(app.presence_tick(t).is_empty(), "no window is dirtied");
            assert!(app.presence_deadline(t).is_none(), "no timer is armed");
        }
        let v = app.presence_view(wid).unwrap();
        assert_eq!(v.cached_row.capacity(), cap_before);
        assert_eq!(cap_before, 0, "a quiet window never painted a row");
        assert_eq!(v.rows, 0);
        assert!(v.ripple_at.is_none());
    }

    /// THE CLASSIFIER GATE (§7): `aterm_phase` runs exactly once per status
    /// revision the slot sees — a thousand refreshes at one revision cost one
    /// call; each revision move costs one more.
    #[test]
    fn the_classifier_runs_once_per_status_revision_never_per_frame() {
        use crate::session_status::{ActivitySample, Evidence};
        let (mut app, _wid, sid, _ctx) = app_with_stub();
        let calls = presence::classifier_calls();
        for _ in 0..1000 {
            app.refresh_presence_session(sid, false);
        }
        assert_eq!(
            presence::classifier_calls() - calls,
            1,
            "one revision, one call"
        );
        let mut revisions = std::collections::BTreeSet::new();
        revisions.insert(app.session_status.revision(sid));
        let t0 = Instant::now();
        let mut ev = Evidence {
            pin: None,
            shell: None,
            lifecycle: None,
            foreground_job: Some(true),
            activity: ActivitySample {
                alt_screen: false,
                content_seq: 1,
                last_input: None,
                last_output: Some(t0),
            },
        };
        // Two observations past the dwell: the slot is minted (revision 1),
        // then Running publishes (revision 2).
        app.session_status.observe(sid, &ev, t0);
        revisions.insert(app.session_status.revision(sid));
        for _ in 0..100 {
            app.refresh_presence_session(sid, false);
        }
        let t1 = t0 + Duration::from_millis(800);
        ev.activity.last_output = Some(t1);
        assert!(
            app.session_status.observe(sid, &ev, t1),
            "Running publishes"
        );
        revisions.insert(app.session_status.revision(sid));
        for _ in 0..100 {
            app.refresh_presence_session(sid, false);
        }
        assert_eq!(
            presence::classifier_calls() - calls,
            revisions.len() as u64,
            "calls == distinct revisions seen ({revisions:?}), not frames"
        );
    }

    // ---- the golden cells ------------------------------------------------

    fn light() -> aterm_render::Theme {
        aterm_render::Theme {
            fg: 0x0020_2020,
            bg: 0x00FA_FAFA,
            cursor: 0x0020_2020,
            selection: 0x00C0_C8FF,
        }
    }

    fn text_of(row: &[RenderCell]) -> String {
        row.iter()
            .map(|c| c.ch)
            .collect::<String>()
            .trim()
            .to_string()
    }

    fn slot(facts: Facts, now: Instant) -> Slot {
        let mut s = Slot::new(now);
        s.absorb(facts, now);
        s
    }

    fn agent(phase: AgentPhase, ctx: Option<u8>) -> Option<AgentReading> {
        Some(AgentReading {
            phase,
            context_pct: ctx,
        })
    }

    fn mail(unread: u64, kind: &str, from: &str) -> presence::MailFacts {
        presence::MailFacts {
            unread,
            head: unread,
            last: (unread > 0).then(|| presence::MailLast {
                kind: kind.into(),
                from: from.into(),
                trust: "agent".into(),
            }),
            ..presence::MailFacts::default()
        }
    }

    /// Every band row of design §1 that this slice paints, as `(name, slot, the
    /// whole line)`. The lines are the mocks' rows through the composer; the
    /// story row is 121 cells, so the whole line is read at 160 columns.
    fn rows(now: Instant) -> Vec<(&'static str, Slot, &'static str)> {
        let role = || Some("worker:claude-satcomp".to_string());
        let mut out = vec![
            (
                "driven",
                slot(
                    Facts {
                        role: role(),
                        revision: 1,
                        agent: agent(AgentPhase::Busy, Some(41)),
                        hand: Hand::DrivenTurn {
                            id: 41,
                            holder: Some("manager".into()),
                        },
                        mail: mail(2, "task", "manager"),
                        link: Link::Connected { rtt_ms: Some(12) },
                        ..Facts::default()
                    },
                    now - Duration::from_secs(192),
                ),
                "worker:claude-satcomp  busy 3m12s  \u{25c2} manager \u{00b7} turn 41  \u{2709}2 task\u{2190}\u{2713}manager  ctx 41%  \u{27df} 12ms",
            ),
            (
                "driving",
                slot(
                    Facts {
                        role: Some("agent:claude-manager".into()),
                        revision: 1,
                        agent: agent(AgentPhase::Busy, Some(77)),
                        hand: Hand::Driving {
                            sid: "s-1e91".into(),
                        },
                        mail: mail(1, "report", "worker"),
                        link: Link::Connected { rtt_ms: Some(5) },
                        ..Facts::default()
                    },
                    now - Duration::from_secs(8),
                ),
                "agent:claude-manager  busy 8s  \u{25b8} @s-1e91  \u{2709}1 report\u{2190}\u{2713}worker  ctx 77%  \u{27df} 5ms",
            ),
            (
                "prompt",
                slot(
                    Facts {
                        role: role(),
                        revision: 1,
                        agent: agent(
                            AgentPhase::Prompt {
                                detail: Some("bash".into()),
                            },
                            Some(38),
                        ),
                        hand: Hand::DrivenLease {
                            holder: "manager".into(),
                        },
                        link: Link::Connected { rtt_ms: Some(4) },
                        ..Facts::default()
                    },
                    now,
                ),
                "worker:claude-satcomp  prompt\u{00b7}bash  \u{25c2} manager  \u{2709}0  ctx 38%  \u{27df} 4ms",
            ),
            (
                "question",
                slot(
                    Facts {
                        role: role(),
                        revision: 1,
                        agent: agent(AgentPhase::Question, Some(12)),
                        link: Link::Connected { rtt_ms: Some(6) },
                        ..Facts::default()
                    },
                    now - Duration::from_secs(120),
                ),
                "worker:claude-satcomp  question 2m00s  \u{2014}  \u{2709}0  ctx 12% \u{26a0}  \u{27df} 6ms",
            ),
            (
                "limited",
                slot(
                    Facts {
                        role: role(),
                        revision: 1,
                        agent: agent(
                            AgentPhase::Limited {
                                reset: Some("19:30".into()),
                                until: Some(now + Duration::from_secs(165_600)),
                            },
                            Some(9),
                        ),
                        mail: mail(3, "task", "manager"),
                        link: Link::Connected { rtt_ms: Some(5) },
                        ..Facts::default()
                    },
                    now - Duration::from_secs(40),
                ),
                "worker:claude-satcomp  limited \u{2192} 19:30 \u{00b7} 1d 22h  \u{2014}  \u{2709}3 task\u{2190}\u{2713}manager  ctx 9% \u{26a0}  \u{27df} 5ms",
            ),
            (
                "hold-fleet-lost",
                slot(
                    Facts {
                        role: role(),
                        revision: 1,
                        agent: agent(AgentPhase::Idle, Some(41)),
                        hold: Some(HoldFact {
                            reason: "fabric-lost".into(),
                            fleet: true,
                        }),
                        mail: presence::MailFacts {
                            unread: 1,
                            queued: 2,
                            head: 1,
                            ..presence::MailFacts::default()
                        },
                        link: Link::Disconnected,
                        ..Facts::default()
                    },
                    now - Duration::from_secs(40),
                ),
                "worker:claude-satcomp  idle 40s  \u{2298} hold fabric-lost \u{00b7}fleet\u{1f512}  \u{2709}1 \u{2191}2  ctx 41%  \u{2715} lost",
            ),
            (
                "attention",
                slot(
                    Facts {
                        role: role(),
                        attention: Some("needs a decision".into()),
                        revision: 1,
                        agent: agent(AgentPhase::Busy, Some(50)),
                        link: Link::Connected { rtt_ms: Some(3) },
                        ..Facts::default()
                    },
                    now,
                ),
                "worker:claude-satcomp  needs a decision  \u{2014}  \u{2709}0  ctx 50%  \u{27df} 3ms",
            ),
            (
                "stalled",
                slot(
                    Facts {
                        role: role(),
                        revision: 1,
                        agent: agent(AgentPhase::Busy, Some(91)),
                        link: Link::Stalled {
                            age_ms: Some(7_400),
                        },
                        ..Facts::default()
                    },
                    now - Duration::from_secs(120),
                ),
                "worker:claude-satcomp  busy 2m00s  \u{2014}  \u{2709}0  ctx 91%  ~ 7s",
            ),
        ];
        // The story row: three settled turns, then a hold that lifted.
        let mut s = Slot::new(now - Duration::from_secs(130));
        let base = now - Duration::from_secs(130);
        for id in 1..=3 {
            s.absorb(
                Facts {
                    role: role(),
                    revision: 1,
                    agent: agent(AgentPhase::Idle, Some(41)),
                    turn: Some(TurnFact {
                        id,
                        settled: true,
                        dur_ms: 10,
                        carried: false,
                    }),
                    mail: mail(1, "note", "h-x"),
                    link: Link::Connected { rtt_ms: Some(12) },
                    ..Facts::default()
                },
                base + Duration::from_secs(id),
            );
        }
        let held = |hold: Option<HoldFact>| Facts {
            role: role(),
            revision: 1,
            agent: agent(AgentPhase::Idle, Some(41)),
            turn: Some(TurnFact {
                id: 3,
                settled: true,
                dur_ms: 10,
                carried: false,
            }),
            mail: mail(1, "note", "h-x"),
            link: Link::Connected { rtt_ms: Some(12) },
            hold,
            ..Facts::default()
        };
        s.absorb(
            held(Some(HoldFact {
                reason: "pause".into(),
                fleet: false,
            })),
            base + Duration::from_secs(10),
        );
        s.absorb(held(None), base + Duration::from_secs(70));
        out.push((
            "story",
            s,
            "worker:claude-satcomp  \u{25c7} quiet since 2m09s \u{00b7} 3 turns \u{00b7} 1 mail \u{00b7} held 1m00s, resumed 1m00s ago  \u{2014}  \u{2709}1 note\u{2190}\u{2713}h-x  ctx 41%  \u{27df} 12ms",
        ));
        out
    }

    /// THE GOLDEN CELLS (§7): every band row at 120, 60 and 24 columns, painted
    /// in light, dark and each stock Windows High-Contrast palette. The 120-col
    /// line is literal; at every width the painted text IS the fitted words
    /// (one margin cell each side), every cell sits on the band, every ink
    /// clears the band, the emoji-capable glyphs are pinned to text
    /// presentation, and hand + phase + mail survive to 24 columns.
    #[test]
    fn golden_cells_for_every_band_row_at_120_60_and_24_in_light_dark_and_hc() {
        let now = Instant::now();
        let palettes: Vec<(
            String,
            Option<crate::chrome_band::ForcedChrome>,
            aterm_render::Theme,
        )> = {
            let mut p = vec![
                ("dark".to_string(), None, aterm_render::Theme::default()),
                ("light".to_string(), None, light()),
            ];
            for (name, hc) in crate::chrome_band::hc_fixtures::STOCK {
                p.push((
                    format!("hc:{name}"),
                    Some(hc),
                    aterm_render::Theme::default(),
                ));
            }
            p
        };
        for (name, s, whole) in rows(now) {
            let w: Words = presence::words(&s, now, 0);
            assert_eq!(w.fit(160), whole, "{name}, whole");
            for cols in [120usize, 60, 24] {
                let fitted = w.fit(cols - 2);
                assert!(
                    fitted.chars().count() <= cols - 2,
                    "{name} at {cols}: {fitted}"
                );
                // hand, phase, mail survive to 24 columns
                let phase_word: String = w.phase.chars().take(4).collect();
                assert!(fitted.contains(&phase_word), "{name} at {cols}: {fitted}");
                assert!(
                    fitted.contains('\u{2709}'),
                    "{name} at {cols}: mail survives: {fitted}"
                );
                let hand_glyph = w.hand.chars().next().unwrap();
                assert!(
                    fitted.contains(hand_glyph),
                    "{name} at {cols}: hand survives: {fitted}"
                );
                for (palette, hc, theme) in &palettes {
                    let paint = || crate::status_bars::paint_presence_row(&w, cols, *theme);
                    let (row, colors) = match hc {
                        Some(hc) => crate::chrome_band::hc_fixtures::with_forced(*hc, || {
                            (paint(), crate::chrome_band::band_colors(*theme))
                        }),
                        None => (paint(), crate::chrome_band::band_colors(*theme)),
                    };
                    assert_eq!(row.len(), cols, "{name} {palette} {cols}");
                    assert_eq!(text_of(&row), fitted, "{name} {palette} {cols}");
                    for cell in &row {
                        assert_eq!(
                            cell.bg, colors.bar_bg,
                            "{name} {palette} {cols}: on the band"
                        );
                        // Every glyph on the row is TEXT: AA 4.5, the hand
                        // and phase hues included (`presence_inks`).
                        if cell.ch != ' ' {
                            assert!(
                                crate::chrome_band::contrast(cell.fg, cell.bg) >= 4.5,
                                "{name} {palette} {cols}: {:?} on {:?} under {:?}",
                                cell.fg,
                                cell.bg,
                                cell.ch
                            );
                        }
                        if matches!(
                            cell.ch,
                            '\u{2713}' | '\u{26a0}' | '\u{2709}' | '\u{2715}' | '\u{1f512}'
                        ) {
                            assert!(
                                cell.text_presentation,
                                "{name} {palette}: {:?} one cell",
                                cell.ch
                            );
                        }
                        assert!(!cell.wide);
                    }
                }
            }
        }
    }

    /// The chip is a LEVEL: the indicator bit spells `Wait`, and the presence
    /// stamp folds the session's level over it — a hold is the filled diamond,
    /// a story the dot, and a quiet session leaves the bit alone.
    #[test]
    fn the_chip_level_folds_the_session_level_over_the_indicator_bit() {
        let (mut app, wid, sid, ctx) = app_with_stub();
        let metadata = |app: &App| -> Vec<crate::tab_bar::TabStripMetadata> {
            let mut m: Vec<_> = app.windows[&wid]
                .tab_set
                .tabs()
                .iter()
                .map(|t| crate::tab_bar::TabStripMetadata::from_presentation(&t.presentation))
                .collect();
            app.stamp_presence_chips(wid, &mut m);
            m
        };
        let front = app.windows[&wid].tab_set.tabs().len() - 1;
        assert_eq!(metadata(&app)[front].attention, ChipLevel::Off);
        assert!(crate::fabric::apply_hold_for_test(
            &ctx,
            Some(crate::fabric::Hold {
                reason: "pause".into(),
                origin: "local".into(),
            })
        ));
        app.refresh_presence_session(sid, false);
        assert_eq!(metadata(&app)[front].attention, ChipLevel::Stop);
        assert_eq!(ChipLevel::Stop.chrome_states(), &["attention", "stop"]);
        assert!(crate::fabric::apply_hold_for_test(&ctx, None));
        app.refresh_presence_session(sid, false);
        assert_eq!(app.presence_level(wid), Level::Story);
        assert_eq!(metadata(&app)[front].attention, ChipLevel::Story);
        app.note_human_acted(wid);
        assert_eq!(metadata(&app)[front].attention, ChipLevel::Off);
        assert_eq!(chip_of_attention(true), ChipLevel::Wait);
        assert!(ChipLevel::Off < ChipLevel::Story && ChipLevel::Story < ChipLevel::Wait);
        assert!(ChipLevel::Wait < ChipLevel::Stop);
    }

    /// THE A11Y ROUND TRIP (§7), the App side: the band's message is the sixth
    /// chrome message, carries the spoken sentence, and sits on bar row 0 —
    /// only while the row is committed.
    #[cfg(a11y_tree)]
    #[test]
    fn the_band_sentence_is_the_sixth_chrome_message_on_bar_row_zero() {
        use crate::accesskit_tree::ChromeMessage;
        let (mut app, wid, _sid, ctx) = app_with_stub();
        assert!(
            app.presence_a11y_message(wid).is_none(),
            "quiet: nothing to say"
        );
        *ctx.turn_lease.lock().unwrap() = Some(crate::Lease::Turn {
            id: 41,
            driver: None,
        });
        app.on_presence_wake(&ctx.self_id, false);
        let m = app.presence_a11y_message(wid).expect("the band speaks");
        assert_eq!(m.message, ChromeMessage::PresenceStatus);
        assert!(m.text.starts_with("driven, turn 41"), "{}", m.text);
        assert_eq!(m.bar_row, Some(0));
        assert!(!m.activates);
        assert!(ChromeMessage::ORDER.contains(&ChromeMessage::PresenceStatus));
    }

    // ---- the adversarial review (round 19, honesty-cost lens) -------------
    //
    // Each test below was written by the review against 9f9df5466, FAILED as
    // the finding said, and passes on the fix it names.

    fn driven_words(now: Instant) -> presence::Words {
        let then = now - Duration::from_secs(192);
        let mut s = Slot::new(then);
        s.absorb(
            Facts {
                role: Some("worker:claude-satcomp".into()),
                revision: 1,
                agent: agent(AgentPhase::Busy, Some(41)),
                hand: Hand::DrivenTurn {
                    id: 41,
                    holder: Some("manager".into()),
                },
                mail: presence::MailFacts {
                    unread: 2,
                    head: 2,
                    last: Some(presence::MailLast {
                        kind: "task".into(),
                        from: "manager".into(),
                        trust: "agent".into(),
                    }),
                    ..presence::MailFacts::default()
                },
                link: Link::Connected { rtt_ms: Some(12) },
                ..Facts::default()
            },
            then,
        );
        presence::words(&s, now, 0)
    }

    /// R1: the story watermark is per (window, SESSION), never per window —
    /// reading tab A's story must not fold tab B's on focus alone.
    #[test]
    fn review_r1_focus_alone_never_reads_another_sessions_story() {
        let (mut app, wid, sid_a, _ctx_a) = app_with_stub();
        let ia = app.windows[&wid].tab_set.tabs().len() - 1;
        for _ in 0..3 {
            app.tell_story(sid_a, StoryVerb::Approval, "").unwrap();
        }
        let sid_b = app.next_session_id;
        app.push_stub_tab(wid, crate::stub_session(sid_b));
        let ib = ia + 1;
        assert_eq!(app.focused_session_id(wid), Some(sid_b));
        app.tell_story(sid_b, StoryVerb::Approval, "").unwrap();
        assert_eq!(app.presence_level(wid), Level::Story, "B: one unread point");
        // The human reads A: switch to it, act while calm.
        app.switch_tab_in(wid, ia);
        assert_eq!(app.focused_session_id(wid), Some(sid_a));
        assert_eq!(app.presence_level(wid), Level::Story);
        app.note_human_acted(wid);
        assert_eq!(app.presence_level(wid), Level::Quiet, "A was read");
        // Focus B. Its point was never read; focus alone must not fold it.
        app.switch_tab_in(wid, ib);
        assert_eq!(app.focused_session_id(wid), Some(sid_b));
        let (text, _) = line(&app, wid);
        assert_eq!(
            app.presence_level(wid),
            Level::Story,
            "B's story was never read, yet the row folded on focus alone: status `{}`, band `{text}`",
            app.presence_status_tail(sid_b)
        );
    }

    /// R1b: the same root — a story the human READ does not come back after
    /// they read a second tab.
    #[test]
    fn review_r1b_a_story_already_read_does_not_return_on_a_tab_switch() {
        let (mut app, wid, sid_a, _ctx_a) = app_with_stub();
        let ia = app.windows[&wid].tab_set.tabs().len() - 1;
        for _ in 0..3 {
            app.tell_story(sid_a, StoryVerb::Approval, "").unwrap();
        }
        let sid_b = app.next_session_id;
        app.push_stub_tab(wid, crate::stub_session(sid_b));
        let ib = ia + 1;
        app.tell_story(sid_b, StoryVerb::Approval, "").unwrap();
        app.switch_tab_in(wid, ia);
        assert_eq!(app.focused_session_id(wid), Some(sid_a));
        app.note_human_acted(wid);
        assert_eq!(app.presence_level(wid), Level::Quiet);
        app.switch_tab_in(wid, ib);
        assert_eq!(app.focused_session_id(wid), Some(sid_b));
        app.note_human_acted(wid);
        assert_eq!(app.presence_level(wid), Level::Quiet, "B read too");
        app.switch_tab_in(wid, ia);
        assert_eq!(app.focused_session_id(wid), Some(sid_a));
        let (text, _) = line(&app, wid);
        assert_eq!(
            app.presence_level(wid),
            Level::Quiet,
            "A's story was read and nothing happened since, yet it is back: band `{text}` status `{}`",
            app.presence_status_tail(sid_a)
        );
        // …and the chips agree: neither tab carries a story dot.
        let mut m: Vec<_> = app.windows[&wid]
            .tab_set
            .tabs()
            .iter()
            .map(|t| crate::tab_bar::TabStripMetadata::from_presentation(&t.presentation))
            .collect();
        app.stamp_presence_chips(wid, &mut m);
        assert_eq!(m[ia].attention, ChipLevel::Off);
        assert_eq!(m[ib].attention, ChipLevel::Off);
    }

    /// R2: a cooperative lease that LAPSES (its driver crashed) posts no wake;
    /// the model arms its expiry as a deadline and the tick re-reads the hand,
    /// so the teal rim and `◂ holder` lift when `lease status` says `none`.
    #[test]
    fn review_r2_a_lapsed_drive_lease_lifts_the_rim() {
        let (mut app, wid, sid, ctx) = app_with_stub();
        let now_us = crate::metrics::now_us();
        *ctx.turn_lease.lock().unwrap() = Some(crate::Lease::Drive {
            holder: "manager".into(),
            expires_us: now_us + 20_000,
        });
        app.on_presence_wake(&ctx.self_id, false);
        assert_eq!(app.presence_level(wid), Level::Driven);
        assert_eq!(line(&app, wid).1, "drive");
        let armed = app
            .presence_deadline(Instant::now())
            .expect("the lease's lapse is a deadline");
        assert!(
            armed <= Instant::now() + Duration::from_millis(20),
            "armed at the lapse, not a second later"
        );
        std::thread::sleep(Duration::from_millis(60));
        assert!(
            !ctx.turn_lease
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .is_live(crate::metrics::now_us()),
            "the lease has lapsed"
        );
        // Every timer the model armed has fired.
        let now = Instant::now();
        let deadline = app.presence_deadline(now);
        let _ = app.presence_tick(deadline.map_or(now, |d| d.max(now)));
        let (text, rim) = line(&app, wid);
        assert_eq!(
            (rim, app.presence_status_tail(sid)),
            ("none", "hand=- level=quiet story=0".to_string()),
            "`lease status` says none but the window says rim={rim} band=`{text}`"
        );
        assert_eq!(app.presence_view(wid).unwrap().rows, 0, "the row folded");
        assert!(
            app.presence_deadline(Instant::now()).is_none(),
            "nothing re-arms on a quiet window"
        );
    }

    /// R3: a turn that TIMES OUT drops the lease while the worker is still
    /// busy; the live phase keeps the slot and the story says the timeout —
    /// never `◇ quiet` over a running worker.
    #[test]
    fn review_r3_a_timed_out_turn_leaves_a_busy_worker_reading_busy() {
        let now = Instant::now();
        let mut s = Slot::new(now);
        s.absorb(
            Facts {
                role: Some("worker:claude-satcomp".into()),
                revision: 1,
                shell: Some(("running", now)),
                agent: agent(AgentPhase::Busy, Some(40)),
                hand: Hand::DrivenTurn {
                    id: 41,
                    holder: Some("manager".into()),
                },
                ..Facts::default()
            },
            now,
        );
        assert_eq!(s.level(0), Level::Driven);
        let later = now + Duration::from_secs(30);
        s.absorb(
            Facts {
                role: Some("worker:claude-satcomp".into()),
                revision: 1,
                shell: Some(("running", now)),
                agent: None,
                hand: Hand::None,
                turn: Some(TurnFact {
                    id: 41,
                    settled: false,
                    dur_ms: 30_000,
                    carried: false,
                }),
                ..Facts::default()
            },
            later,
        );
        assert_eq!(s.level(0), Level::Story, "the timeout is a story point");
        let w = presence::words(&s, later + Duration::from_secs(1), 0);
        assert!(
            w.phase.starts_with("busy") && w.sentence.contains("timed out"),
            "level={:?} phase=`{}` since={:?} sentence=`{}`",
            s.level(0),
            w.phase,
            w.since,
            w.sentence
        );
        assert_eq!(
            w.since,
            vec![
                "31s".to_string(),
                "1 turn".to_string(),
                "1 timed out".to_string()
            ]
        );
        assert_eq!(w.sentence, "busy, 1 turn, 1 timed out, context 40 percent");
        // The same story on an IDLE worker is the quiet row.
        s.absorb(
            Facts {
                role: Some("worker:claude-satcomp".into()),
                revision: 2,
                shell: Some(("idle", later)),
                agent: agent(AgentPhase::Idle, Some(40)),
                turn: Some(TurnFact {
                    id: 41,
                    settled: false,
                    dur_ms: 30_000,
                    carried: false,
                }),
                ..Facts::default()
            },
            later + Duration::from_secs(2),
        );
        let w = presence::words(&s, later + Duration::from_secs(3), 0);
        assert_eq!(w.phase, "\u{25c7} quiet");
        assert_eq!(
            w.sentence,
            "quiet, since 3s, 1 turn, 1 timed out, context 40 percent"
        );
    }

    /// R4: an unread TASK still waits after a later note lands — the wait
    /// rim stays, and the mail slot names the task, not the note.
    #[test]
    fn review_r4_an_unread_task_still_waits_after_a_later_note() {
        let (mut app, wid, sid, ctx) = app_with_stub();
        let fabric_sid = ctx.self_id.as_str();
        let reply = crate::fabric::cmd_deliver(
            &app.store,
            &format!("{fabric_sid} off=1 from=h-x kind=task trust=human text=please"),
        );
        assert!(reply.starts_with("OK"), "{reply}");
        app.on_presence_wake(&ctx.self_id, false);
        assert_eq!(app.presence_level(wid), Level::Attention);
        assert_eq!(line(&app, wid).1, "wait");
        let reply = crate::fabric::cmd_deliver(
            &app.store,
            &format!("{fabric_sid} off=2 from=s-y kind=note trust=agent text=fyi"),
        );
        assert!(reply.starts_with("OK"), "{reply}");
        app.on_presence_wake(&ctx.self_id, false);
        let (text, rim) = line(&app, wid);
        assert_eq!(
            (app.presence_level(wid), rim),
            (Level::Attention, "wait"),
            "the task from h-x is still unread; band `{text}`, status `{}`",
            app.presence_status_tail(sid)
        );
        assert!(
            text.contains("\u{2709}2 task\u{2190}\u{2713}h-x"),
            "the slot names the waiting task: {text}"
        );
    }

    /// R5: a `since` that prints seconds — `3m12s` included — ticks every
    /// second, never once a minute with the figure stale by up to 59 s.
    #[test]
    fn review_r5_a_since_that_prints_seconds_ticks_every_second() {
        let (mut app, wid, _sid, _ctx) = app_with_stub();
        let now = Instant::now();
        let w = driven_words(now);
        assert_eq!(w.since, vec!["3m12s".to_string()]);
        {
            let ws = app.windows.get_mut(&wid).unwrap();
            ws.presence.words = Some(w);
            ws.presence.rows = 1;
        }
        let d = app.presence_deadline(now).expect("a row is up");
        assert!(
            d <= now + Duration::from_secs(1),
            "`busy 3m12s` changes at +1 s but the next tick is at +{:?}",
            d.saturating_duration_since(now)
        );
        assert!(prints_seconds("3m12s"));
        assert!(prints_seconds("since 40s"));
        assert!(prints_seconds("held 1m00s, resumed 1m00s ago"));
        assert!(!prints_seconds("2h05m"));
        assert!(!prints_seconds("1d 22h"));
        assert!(!prints_seconds("3 turns"));
        assert!(!prints_seconds("\u{2192} 19:30"));
    }

    /// R6: the limited figure counts DOWN to the reset (design §1, the mock),
    /// with the design's separator — and prints nothing when the reset cannot
    /// be placed on the clock, never the time since the limit began.
    #[test]
    fn review_r6_the_limited_figure_counts_down_to_the_reset() {
        let now = Instant::now();
        let mut s = Slot::new(now);
        s.absorb(
            Facts {
                role: Some("worker:claude-satcomp".into()),
                revision: 1,
                agent: agent(
                    AgentPhase::Limited {
                        reset: Some("19:30".into()),
                        until: Some(now + Duration::from_secs(2 * 3600)),
                    },
                    Some(9),
                ),
                ..Facts::default()
            },
            now,
        );
        let a = presence::words(&s, now + Duration::from_secs(60), 0);
        let b = presence::words(&s, now + Duration::from_secs(120), 0);
        assert_eq!(
            a.since,
            vec!["\u{2192} 19:30".to_string(), "1h59m".to_string()]
        );
        assert_eq!(
            b.since,
            vec!["\u{2192} 19:30".to_string(), "1h58m".to_string()]
        );
        assert!(
            a.fit(160).contains("limited \u{2192} 19:30 \u{00b7} 1h59m"),
            "{}",
            a.fit(160)
        );
        assert_eq!(a.sentence, "limited, resets 19:30, context 9 percent");
        // Unplaceable: the reset alone, no figure.
        let mut u = Slot::new(now);
        u.absorb(
            Facts {
                revision: 1,
                agent: agent(
                    AgentPhase::Limited {
                        reset: Some("19:30".into()),
                        until: None,
                    },
                    None,
                ),
                ..Facts::default()
            },
            now,
        );
        let w = presence::words(&u, now + Duration::from_secs(120), 0);
        assert_eq!(w.since, vec!["\u{2192} 19:30".to_string()], "{w:?}");
        assert!(!w.fit(160).contains("2m00s"), "{}", w.fit(160));
        // The reset outlives the countdown under elision (a stop clause).
        let narrow = a.fit(40);
        assert!(narrow.contains("\u{2192} 19:30"), "{narrow}");
        assert!(!narrow.contains("1h59m"), "{narrow}");
    }

    #[test]
    fn review_r6b_the_limited_row_keeps_the_designs_separator() {
        let now = Instant::now();
        let mut s = Slot::new(now);
        s.absorb(
            Facts {
                revision: 1,
                agent: agent(
                    AgentPhase::Limited {
                        reset: Some("19:30".into()),
                        until: Some(now + Duration::from_secs(165_600)),
                    },
                    None,
                ),
                ..Facts::default()
            },
            now,
        );
        let w = presence::words(&s, now, 0);
        assert!(
            w.fit(160)
                .contains("limited \u{2192} 19:30 \u{00b7} 1d 22h"),
            "design §1 / mock: `limited → 19:30 · 1d 22h`; got `{}`",
            w.fit(160)
        );
    }

    /// R7: `chrome band=` is the row the human sees — fitted at the painter's
    /// width (one margin cell each side), never a slot past the margin.
    #[test]
    fn review_r7_chrome_band_is_the_row_the_human_sees() {
        let (mut app, wid, _sid, _ctx) = app_with_stub();
        let now = Instant::now();
        let w = driven_words(now);
        let whole = w.fit(160);
        let cols = whole.chars().count() as u16;
        {
            let ws = app.windows.get_mut(&wid).unwrap();
            ws.presence.words = Some(w.clone());
            ws.presence.rows = 1;
            ws.cols = cols;
        }
        let painted = crate::status_bars::paint_presence_row(
            &w,
            usize::from(cols),
            aterm_render::Theme::default(),
        );
        let (band, _) = app.presence_report(wid).unwrap();
        assert_eq!(
            band,
            text_of(&painted),
            "at {cols} columns the wire and the row disagree"
        );
        assert!(
            !band.contains("\u{27df}"),
            "the rtt fell off the row: {band}"
        );
    }

    /// R8: a turn is attributed only to the session the dispatch resolved as
    /// its driver (`Lease::Turn::driver`, the source of the edge the turn
    /// came over) — a read-screen observer watching the session cannot have
    /// typed it, and neither can a write edge that merely stands (see
    /// `adv3_…` for the seam itself).
    #[test]
    fn review_r8_a_turn_is_not_attributed_to_a_read_only_observer() {
        let (mut app, wid, sid_a, ctx_a) = app_with_stub();
        let ia = app.windows[&wid].tab_set.tabs().len() - 1;
        let sid_b = app.next_session_id;
        app.push_stub_tab(wid, crate::stub_session(sid_b));
        let ctx_b = app.pool.get(sid_b).unwrap().ctx.clone();
        ctx_b.meta.lock().unwrap().role = Some("watcher".into());
        {
            let mut edges = ctx_a.edges.lock().unwrap();
            let _ = edges.grant(
                ctx_b.self_id.clone(),
                ctx_a.self_id.clone(),
                aterm_session::Op::ReadScreen,
                ctx_a.nonce,
            );
        }
        app.switch_tab_in(wid, ia);
        assert_eq!(app.focused_session_id(wid), Some(sid_a));
        // A turn over the Owner token (the CLI): no write edge is involved.
        *ctx_a.turn_lease.lock().unwrap() = Some(crate::Lease::Turn {
            id: 41,
            driver: None,
        });
        app.on_presence_wake(&ctx_a.self_id, false);
        let (text, _) = line(&app, wid);
        assert!(
            !text.contains("watcher"),
            "a read-screen edge cannot type a turn, yet the band names it: `{text}` (status `{}`)",
            app.presence_status_tail(sid_a)
        );
        assert!(text.contains("\u{25c2} turn 41"), "{text}");
        assert!(
            app.presence_status_tail(sid_a).starts_with("hand=turn:41 "),
            "{}",
            app.presence_status_tail(sid_a)
        );
        // A WRITE edge from the same session changes nothing by itself: the
        // lease still names nobody.
        {
            let mut edges = ctx_a.edges.lock().unwrap();
            let _ = edges.grant(
                ctx_b.self_id.clone(),
                ctx_a.self_id.clone(),
                aterm_session::Op::WriteInput,
                ctx_a.nonce,
            );
        }
        app.refresh_presence_session(sid_a, false);
        let (text, _) = line(&app, wid);
        assert!(text.contains("\u{25c2} turn 41"), "{text}");
        assert!(!text.contains("watcher"), "{text}");
        // A turn whose lease names that session as its driver: now it is.
        *ctx_a.turn_lease.lock().unwrap() = Some(crate::Lease::Turn {
            id: 42,
            driver: Some(ctx_b.self_id.clone()),
        });
        app.refresh_presence_session(sid_a, false);
        let (text, _) = line(&app, wid);
        assert!(text.contains("\u{25c2} watcher \u{00b7} turn 42"), "{text}");
        assert!(
            app.presence_status_tail(sid_a)
                .starts_with("hand=turn:42:watcher "),
            "{}",
            app.presence_status_tail(sid_a)
        );
    }

    /// R9: a cache hit on the frame path returns the cached row itself — no
    /// clone per composed frame while a row is up.
    #[test]
    fn review_r9_a_cache_hit_on_the_frame_path_does_not_allocate() {
        let (mut app, wid, _sid, ctx) = app_with_stub();
        assert!(crate::fabric::apply_hold_for_test(
            &ctx,
            Some(crate::fabric::Hold {
                reason: "pause".into(),
                origin: "local".into(),
            })
        ));
        app.on_presence_wake(&ctx.self_id, false);
        let theme = aterm_render::Theme::default();
        let first_len = app.presence_band_row(wid, 120, theme).len();
        assert_eq!(first_len, 120);
        let cached = app.presence_view(wid).unwrap().cached_row.as_ptr();
        let second = app.presence_band_row(wid, 120, theme);
        assert_eq!(
            second.as_ptr(),
            cached,
            "a cache hit returned a fresh {}-cell Vec (a clone per composed frame)",
            second.len()
        );
    }

    /// R10: U+1F512 (the fleet lock) is pinned to text presentation like the
    /// other emoji-capable glyphs, so it stays one cell.
    #[test]
    fn review_r10_the_fleet_lock_is_pinned_to_one_cell() {
        let now = Instant::now();
        let mut s = Slot::new(now);
        s.absorb(
            Facts {
                revision: 1,
                agent: agent(AgentPhase::Idle, None),
                hold: Some(HoldFact {
                    reason: "fabric-lost".into(),
                    fleet: true,
                }),
                ..Facts::default()
            },
            now,
        );
        let w = presence::words(&s, now, 0);
        let row = crate::status_bars::paint_presence_row(&w, 120, aterm_render::Theme::default());
        let at = row
            .iter()
            .position(|c| c.ch == '\u{1f512}')
            .expect("the lock is on the row");
        assert!(
            aterm_grapheme::is_emoji_presentation('\u{1f512}'),
            "U+1F512 defaults to emoji presentation"
        );
        assert!(
            row[at].text_presentation,
            "cell {at}: wide={} text_presentation={} emoji_presentation={}",
            row[at].wide, row[at].text_presentation, row[at].emoji_presentation
        );
        assert!(!row[at].wide);
    }

    /// R11: the link's RECOVERY reaches the fabric slot — the tick re-reads
    /// the facts of a session with a row up (and the bridge lane posts a
    /// presence wake on the up transition; a headless App has no proxy, so the
    /// tick is what this test can exercise). A note waits unread so the row
    /// stays up across the stall: a session with nothing else to say folds
    /// its row when the link is back, as it would for any state that ended.
    #[test]
    fn review_r11_a_recovered_link_reaches_the_fabric_slot() {
        // The link is process-global: attaching a bridge OUTSIDE the section
        // bumped the generation under a sibling's measurement (adv2b's
        // 2.1 s stall read its own `link down` as stale, 2026-09-19).
        crate::fabric::with_link_reset(review_r11_a_recovered_link_reaches_the_fabric_slot_body);
    }

    fn review_r11_a_recovered_link_reaches_the_fabric_slot_body() {
        let (mut app, wid, _sid, ctx) = app_with_stub();
        let g = crate::fabric::next_bridge_generation();
        crate::fabric::bridge_attached(g);
        assert!(crate::fabric::link_report(
            &app.store,
            Some(g),
            true,
            "",
            Some(12)
        ));
        let reply = crate::fabric::cmd_deliver(
            &app.store,
            &format!(
                "{} off=1 from=s-y kind=note trust=agent text=fyi",
                ctx.self_id.as_str()
            ),
        );
        assert!(reply.starts_with("OK"), "{reply}");
        app.on_presence_wake(&ctx.self_id, false);
        let (text, _) = line(&app, wid);
        assert!(text.contains("\u{27df} 12ms"), "connected: {text}");
        assert!(crate::fabric::link_report(
            &app.store,
            Some(g),
            false,
            "broker closed",
            None
        ));
        app.on_presence_wake(&ctx.self_id, false);
        let (text, _) = line(&app, wid);
        assert!(text.contains("~ "), "stalled: {text}");
        assert!(crate::fabric::link_report(
            &app.store,
            Some(g),
            true,
            "",
            Some(9)
        ));
        assert_eq!(crate::fabric::fabric_state(), "connected");
        let now = Instant::now();
        for _ in 0..3 {
            if let Some(d) = app.presence_deadline(now) {
                let _ = app.presence_tick(d.max(now) + Duration::from_millis(1));
            }
        }
        let (text, _) = line(&app, wid);
        assert!(
            text.contains("\u{27df} 9ms"),
            "the bridge is back (fabric={}) but the band says `{text}` (level {:?})",
            crate::fabric::fabric_state(),
            app.presence_level(wid)
        );
        assert!(!text.contains("~ "), "{text}");
    }

    /// R12: the role slot cannot spell a hold, a hand or a mail count — the
    /// band's own glyphs are stripped from every free-text value and runs of
    /// spaces (the slot separator) collapse to one.
    #[test]
    fn review_r12_the_role_slot_cannot_spell_a_hold() {
        let now = Instant::now();
        let mut s = Slot::new(now);
        s.absorb(
            Facts {
                role: Some(
                    "\u{2298} hold pause \u{00b7}fleet\u{1f512}  \u{25c2} manager \u{00b7} turn 9"
                        .into(),
                ),
                revision: 1,
                agent: agent(AgentPhase::Idle, None),
                ..Facts::default()
            },
            now,
        );
        let w = presence::words(&s, now, 0);
        let fitted = w.fit(120);
        assert!(
            !fitted.contains("\u{2298} hold") && !fitted.contains("\u{25c2} manager"),
            "level={:?} band=`{fitted}` sentence=`{}`",
            s.level(0),
            w.sentence
        );
        assert_eq!(w.role, "hold pause fleet manager turn 9");
        assert!(!fitted.contains("  hold"), "no forged slot gap: {fitted}");
        assert_eq!(
            presence::sanitize_token("\u{2709}9 task\u{2190}\u{2713}boss", 32),
            "9 taskboss"
        );
        assert_eq!(presence::sanitize_token("  a   b  ", 32), "a b");
    }

    /// Observed live: a told word under a hold read `✓ approved 0s ⊘ hold
    /// review` — the stop's duration behind the told word, as an age.
    #[test]
    fn a_told_word_under_a_hold_carries_no_stop_duration() {
        let (mut app, wid, sid, ctx) = app_with_stub();
        // The slot's baseline first, so the hold that follows is news (a hold
        // already standing at the mint is state — `adv7`).
        app.refresh_presence_session(sid, false);
        assert!(crate::fabric::apply_hold_for_test(
            &ctx,
            Some(crate::fabric::Hold {
                reason: "review".into(),
                origin: "local".into(),
            })
        ));
        app.on_presence_wake(&ctx.self_id, false);
        assert_eq!(app.tell_story(sid, StoryVerb::Approval, ""), Ok(2));
        let (text, _) = line(&app, wid);
        assert!(
            text.contains("\u{2713} approved  \u{2298} hold review"),
            "{text}"
        );
        assert!(!text.contains("approved 0s"), "{text}");
    }

    // ───────── the second adversarial review (honesty-cost), 2026-09-19 ─────────

    /// ADV-1: a turn ledger CARRIED across a self-update handoff
    /// (`handoff_carry.rs`: every record `carried=1`) describes a turn that
    /// settled in the previous process. The slot's first sight of it must not
    /// be narrated as news — `Slot::absorb` used to note a `Turn` whenever
    /// `self.turn` was `None` and the facts held one, so after every
    /// self-update each window whose session ever had a turn showed `◇ quiet
    /// since 0s · 1 turn`, a violet story dot, and a Success-toned row, for a
    /// turn nobody drove since the human last looked. A carried record is the
    /// ledger's baseline (`TurnFact::carried`); a turn settled in THIS
    /// process is still news.
    #[test]
    fn adv1_a_carried_turn_ledger_is_not_a_fresh_story() {
        // The level read below folds the process-global link state in
        // (`Note` under a sibling test's attached bridge): take the section.
        crate::fabric::with_link_reset(adv1_a_carried_turn_ledger_is_not_a_fresh_story_body);
    }

    fn adv1_a_carried_turn_ledger_is_not_a_fresh_story_body() {
        let (mut app, wid, sid, ctx) = app_with_stub();
        {
            let mut turns = ctx.turns.lock().unwrap();
            *turns = crate::turn_ledger::TurnLedger::carried(
                vec![crate::turn_ledger::TurnRecord {
                    id: 7,
                    started_ms: 0,
                    dur_ms: 1200,
                    submitted: true,
                    status: "settled",
                    text: String::new(),
                    screen_hash: 0,
                    seq: 0,
                    arch: Default::default(),
                    carried: true,
                }],
                0,
            );
        }
        // The new process's first refresh of the adopted session (the status
        // observer's first revision, or any wake).
        app.refresh_presence_session(sid, false);
        app.refresh_presence_window(wid);
        let slot = app.presence.slot(sid).expect("slot");
        assert_eq!(
            slot.story_seq,
            0,
            "a carried turn was narrated as a story point: level={:?} chrome=`{}`",
            app.presence_level(wid),
            app.presence_chrome_line()
        );
        assert_eq!(app.presence_level(wid), Level::Quiet);
        assert!(
            app.presence.slot(sid).unwrap().settled_at.is_none(),
            "no Success glow for a turn that settled in another process"
        );
        // A turn settled in THIS process, on top of the carried baseline, is
        // news: the story counts it and the tone glows.
        {
            let mut turns = ctx.turns.lock().unwrap();
            turns.push(crate::turn_ledger::TurnRecord {
                id: 8,
                started_ms: 0,
                dur_ms: 900,
                submitted: true,
                status: "settled",
                text: String::new(),
                screen_hash: 0,
                seq: 0,
                arch: Default::default(),
                carried: false,
            });
        }
        app.refresh_presence_session(sid, false);
        app.refresh_presence_window(wid);
        assert_eq!(app.presence.slot(sid).unwrap().story_seq, 1);
        assert_eq!(app.presence_level(wid), Level::Story);
        assert!(
            app.presence_chrome_line().contains("1 turn"),
            "{}",
            app.presence_chrome_line()
        );
    }

    /// ADV-2: the fabric slot's `~ <age>s` must be the age of the STALL. Two
    /// ways it was not: (a) a freshly attached bridge that has never acked
    /// (`bridge_attached` clears `acked_at`) read `~ 0s` for as long as it
    /// dialed — "bridge stalled 0 seconds" after ten minutes; (b) after a
    /// `link down`, `fabric_link_facts().2` is the time since the last ack —
    /// which a quiet link never refreshes — so a link that was up for an hour
    /// read `~ 3600s` one second into its stall. Now: a bridge still dialing
    /// prints `~` with no figure (`fabric_stalled_ms()` is `None`), and a
    /// stall is dated from its own transition.
    #[test]
    fn adv2_the_stall_age_is_the_stalls_not_the_links() {
        let (mut app, wid, _sid, ctx) = app_with_stub();
        crate::fabric::with_link_reset(|| {
            let g = crate::fabric::next_bridge_generation();
            crate::fabric::bridge_attached(g);
            // A note keeps a row up whatever the link does.
            let reply = crate::fabric::cmd_deliver(
                &app.store,
                &format!(
                    "{} off=1 from=s-y kind=note trust=agent text=fyi",
                    ctx.self_id.as_str()
                ),
            );
            assert!(reply.starts_with("OK"), "{reply}");
            app.on_presence_wake(&ctx.self_id, false);
            let (text, _) = line(&app, wid);
            let sentence = app
                .presence_view(wid)
                .unwrap()
                .words
                .as_ref()
                .unwrap()
                .sentence
                .clone();
            assert!(
                !text.contains("~ 0s"),
                "(a) a bridge that never acked has no stall age, yet: `{text}` / `{sentence}`"
            );
            assert!(
                text.ends_with("  ~"),
                "the glyph alone, no figure: `{text}`"
            );
            assert!(
                sentence.ends_with("bridge stalled"),
                "no figure spoken: `{sentence}`"
            );
            assert_eq!(crate::fabric::fabric_stalled_ms(), None);
        });
    }

    /// ADV-2b: after a `link down`, the age is the time since the last `link
    /// up` REPORT (sent on change or a 2x rtt move, never on a timer): a link
    /// that was up for two seconds reads `~ 2s` the moment it stalls.
    #[test]
    fn adv2b_a_fresh_stall_is_not_as_old_as_the_link_was() {
        let (mut app, wid, _sid, ctx) = app_with_stub();
        crate::fabric::with_link_reset(|| {
            let g = crate::fabric::next_bridge_generation();
            crate::fabric::bridge_attached(g);
            let reply = crate::fabric::cmd_deliver(
                &app.store,
                &format!(
                    "{} off=1 from=s-y kind=note trust=agent text=fyi",
                    ctx.self_id.as_str()
                ),
            );
            assert!(reply.starts_with("OK"), "{reply}");
            assert!(crate::fabric::link_report(
                &app.store,
                Some(g),
                true,
                "",
                Some(12)
            ));
            std::thread::sleep(Duration::from_millis(2100));
            assert!(crate::fabric::link_report(
                &app.store,
                Some(g),
                false,
                "no-ack",
                None
            ));
            app.on_presence_wake(&ctx.self_id, false);
            let (text, _) = line(&app, wid);
            let sentence = app
                .presence_view(wid)
                .unwrap()
                .words
                .as_ref()
                .unwrap()
                .sentence
                .clone();
            assert!(
                text.contains("~ 0s"),
                "the stall is 0 s old, the link was up 2 s, the band says: `{text}` / `{sentence}` (status fabric_link_age_ms={:?})",
                crate::fabric::fabric_link_facts().2
            );
            assert!(sentence.ends_with("bridge stalled 0 seconds"), "{sentence}");
            // `status`'s own `fabric_link_age_ms=` still says how long ago the
            // last ack was — a different fact, unchanged.
            assert!(
                crate::fabric::fabric_link_facts()
                    .2
                    .is_some_and(|ms| ms >= 2000)
            );
            assert!(crate::fabric::fabric_stalled_ms().is_some_and(|ms| ms < 1000));
            // A second `down` (the reason moved) is the same stall: its date
            // holds. The link coming back clears it.
            assert!(crate::fabric::link_report(
                &app.store,
                Some(g),
                false,
                "refused",
                None
            ));
            assert!(crate::fabric::fabric_stalled_ms().is_some_and(|ms| ms < 1000));
            assert!(crate::fabric::link_report(
                &app.store,
                Some(g),
                true,
                "",
                Some(9)
            ));
            assert_eq!(crate::fabric::fabric_stalled_ms(), None);
        });
    }

    /// ADV-3: `cmd_turn` — the seam every `turn` verb passes — used to carry
    /// no issuer, and `Lease::Turn(id)` recorded none; the band named whoever
    /// held a WRITE edge into the session. So an Owner-token turn (the CLI, a
    /// human at another instance) while a manager's connection stood was
    /// printed `◂ manager · turn N` — "driven by manager" — and `status
    /// hand=turn:N:manager` said the same to every agent. Now the lease names
    /// its driver from the dispatch's scope: an Owner turn names nobody, an
    /// edge-scoped turn names the edge's source — and the driver's own band
    /// says `▸ @<sid>` for exactly the length of that turn.
    #[test]
    fn adv3_an_owner_turn_is_not_credited_to_a_standing_write_edge() {
        let (mut app, wid, sid_a, ctx_a) = app_with_stub();
        let ia = app.windows[&wid].tab_set.tabs().len() - 1;
        let sid_b = app.next_session_id;
        app.push_stub_tab(wid, crate::stub_session(sid_b));
        let ib = app.windows[&wid].tab_set.tabs().len() - 1;
        let ctx_b = app.pool.get(sid_b).unwrap().ctx.clone();
        ctx_b.meta.lock().unwrap().role = Some("manager".into());
        {
            let mut edges = ctx_a.edges.lock().unwrap();
            let _ = edges.grant(
                ctx_b.self_id.clone(),
                ctx_a.self_id.clone(),
                aterm_session::Op::WriteInput,
                ctx_a.nonce,
            );
        }
        app.switch_tab_in(wid, ia);
        assert_eq!(app.focused_session_id(wid), Some(sid_a));
        // A real `turn` on A through the lease seam, with the driver the
        // dispatch resolved: `None` is the Owner CLI's `aterm ctl @A turn …`
        // (its scope check passes, and it is nobody on the fabric).
        let term_a = app.pool.get(sid_a).unwrap().term.clone();
        let store = app.store.clone();
        let run_turn = |driver: Option<SessionId>| {
            let term = term_a.clone();
            let store = store.clone();
            let ctx = ctx_a.clone();
            std::thread::spawn(move || {
                let subscribers = crate::subscribe::new_registry();
                let paste = |_: &str| true;
                let press = |_: &str| true;
                crate::control::cmd_turn(
                    &term,
                    &store,
                    sid_a,
                    "idle=300 timeout=4000 submit=none -- hello",
                    &subscribers,
                    &ctx,
                    &crate::control::TurnIo {
                        paste: &paste,
                        press: &press,
                        driver,
                        ..crate::control::TurnIo::paste_only()
                    },
                )
            })
        };
        let wait_for_lease = |ctx: &crate::SessionCtx| -> u64 {
            for _ in 0..200 {
                if let Some(crate::Lease::Turn { id, .. }) = ctx.turn_lease.lock().unwrap().as_ref()
                {
                    return *id;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            panic!("the turn never held the lease");
        };

        // 1. The Owner's turn: no driver, whatever edges stand.
        let runner = run_turn(None);
        let id = wait_for_lease(&ctx_a);
        app.refresh_presence_session(sid_a, false);
        let (text, _) = line(&app, wid);
        let tail = app.presence_status_tail(sid_a);
        let sentence = app
            .presence_view(wid)
            .unwrap()
            .words
            .as_ref()
            .map(|w| w.sentence.clone())
            .unwrap_or_default();
        assert!(runner.join().unwrap().starts_with("OK"));
        assert!(
            !text.contains("manager"),
            "an Owner-token turn {id} is credited to the standing write edge: band=`{text}` status=`{tail}` sentence=`{sentence}`"
        );
        assert!(text.contains(&format!("\u{25c2} turn {id}")), "{text}");
        assert!(tail.starts_with(&format!("hand=turn:{id} ")), "{tail}");
        assert!(
            sentence.starts_with(&format!("driven, turn {id}")),
            "{sentence}"
        );
        // Settled: the lease is gone and the hand with it.
        app.refresh_presence_session(sid_a, false);
        assert!(app.presence_status_tail(sid_a).starts_with("hand=- "));

        // 2. The manager's turn over its edge: the edge's source is the
        // driver, named by its role — and the manager's own band says `▸ @A`.
        let runner = run_turn(Some(ctx_b.self_id.clone()));
        let id = wait_for_lease(&ctx_a);
        app.refresh_presence_session(sid_a, false);
        app.refresh_presence_session(sid_b, false);
        let (text, _) = line(&app, wid);
        assert!(
            text.contains(&format!("\u{25c2} manager \u{00b7} turn {id}")),
            "{text}"
        );
        assert!(
            app.presence_status_tail(sid_a)
                .starts_with(&format!("hand=turn:{id}:manager "))
        );
        app.switch_tab_in(wid, ib);
        app.refresh_presence_window(wid);
        let (text_b, _) = line(&app, wid);
        let short = presence::short_sid(ctx_a.self_id.as_str());
        assert!(
            text_b.contains(&format!("\u{25b8} @{short}")),
            "the driver's band: `{text_b}`"
        );
        assert!(
            app.presence_status_tail(sid_b).starts_with("hand=driving:"),
            "{}",
            app.presence_status_tail(sid_b)
        );
        assert!(runner.join().unwrap().starts_with("OK"));
        // The turn settled: the manager drives nobody now, edge or no edge.
        app.refresh_presence_session(sid_b, false);
        assert!(
            app.presence_status_tail(sid_b).starts_with("hand=- "),
            "{}",
            app.presence_status_tail(sid_b)
        );
    }

    /// ADV-4: the hand slot is TEXT (`◂ manager · turn 41`, `⊘ hold review
    /// ·fleet🔒`, bold) painted in the presence hues. `presence_tones` floors
    /// them at 3:1 — the non-text floor, right for the rim — and the row used
    /// to paint its words in exactly those: 3.4:1 for the hold's ink on the
    /// default dark theme, 3.3:1 for drive and wait on a light one, while
    /// every other ink on the row (`label`, `value`, `warn`) is AA 4.5:1. The
    /// words are painted in `presence_inks` now — the same hues at the text
    /// floor — and the rim keeps its own.
    #[test]
    fn adv4_the_hand_slots_words_meet_the_rows_own_text_floor() {
        use crate::chrome_band::{band_colors, contrast, presence_inks, presence_tones};
        let mut failures = Vec::new();
        for (name, theme) in [("dark", aterm_render::Theme::default()), ("light", light())] {
            let c = band_colors(theme);
            let p = presence_inks(theme);
            for (tone, ink) in [
                ("drive", p.drive),
                ("wait", p.wait),
                ("stop", p.stop),
                ("story", p.story),
            ] {
                let r = contrast(ink, c.bar_bg);
                if r < 4.5 {
                    failures.push(format!(
                        "{name} {tone}: {ink:?} on {:?} = {r:.2}:1",
                        c.bar_bg
                    ));
                }
            }
            // The rim's tones are the non-text floor, untouched.
            let t = presence_tones(theme);
            for ink in [t.drive, t.wait, t.stop, t.story] {
                assert!(contrast(ink, c.bar_bg) >= 3.0);
            }
        }
        assert!(
            failures.is_empty(),
            "text below AA on the band:\n{}",
            failures.join("\n")
        );
        // And on the painted row itself: every non-blank cell of every golden
        // row clears AA (the golden test pins this per palette too).
        let now = Instant::now();
        for (name, s, _) in rows(now) {
            let w = presence::words(&s, now, 0);
            for theme in [aterm_render::Theme::default(), light()] {
                let row = crate::status_bars::paint_presence_row(&w, 120, theme);
                for cell in row.iter().filter(|c| c.ch != ' ') {
                    assert!(
                        contrast(cell.fg, cell.bg) >= 4.5,
                        "{name}: {:?} under {:?} = {:.2}:1",
                        cell.fg,
                        cell.ch,
                        contrast(cell.fg, cell.bg)
                    );
                }
            }
        }
    }

    /// ADV-5: the idle invariant with the worker's status revisions replayed
    /// — a Running/Quiet oscillation on an otherwise quiet session must leave
    /// `presence_fp` at 0 and paint no row.
    #[test]
    fn adv5_presence_fp_stays_zero_while_status_revisions_replay() {
        use crate::session_status::{ActivitySample, Evidence};
        let (mut app, wid, sid, _ctx) = app_with_stub();
        crate::fabric::with_link_reset(|| {
            app.refresh_presence_session(sid, false);
            app.refresh_presence_all_windows();
            let t0 = Instant::now();
            let mut ev = Evidence {
                pin: None,
                shell: None,
                lifecycle: None,
                foreground_job: Some(true),
                activity: ActivitySample {
                    alt_screen: false,
                    content_seq: 1,
                    last_input: None,
                    last_output: Some(t0),
                },
            };
            let mut revisions = 0u64;
            let mut last = app.session_status.revision(sid);
            for i in 0..1000u64 {
                let t = t0 + Duration::from_millis(i * 700);
                ev.activity.content_seq += 1;
                ev.activity.last_output = if i % 6 < 3 { Some(t) } else { None };
                ev.foreground_job = Some(i % 6 < 3);
                app.session_status.observe(sid, &ev, t);
                let r = app.session_status.revision(sid);
                if r != last {
                    revisions += 1;
                    last = r;
                    app.refresh_presence_session(sid, false);
                }
                assert_eq!(
                    app.presence_fp(wid, t),
                    0,
                    "frame {i}: {}",
                    app.presence_chrome_line()
                );
                assert!(app.presence_overlay(wid, t).is_none());
                assert!(app.presence_tick(t).is_empty());
            }
            assert!(revisions > 0, "the replay moved no revision ({revisions})");
            assert_eq!(app.presence_view(wid).unwrap().cached_row.capacity(), 0);
            eprintln!("adv5: {revisions} revisions replayed, fp stayed 0");
        });
    }

    /// ADV-8 (measure, C1): the frame path's three presence reads per frame,
    /// quiet vs a hold row+rim, in ns/frame. Measured before the fix: quiet
    /// `overlay=1916 ns` of which `presence_tones()=1849 ns` — the four
    /// contrast-floored tones derived before the rim was matched, per
    /// composed frame per window. The quiet overlay answers before the tones
    /// now; the one assertion here is structural (a quiet overlay is `None`
    /// and the fp is 0), the numbers are printed for the record.
    #[test]
    fn adv8_measure_the_frame_paths_presence_cost() {
        let (mut app, wid, _sid, ctx) = app_with_stub();
        let theme = aterm_render::Theme::default();
        let bench = |app: &mut App, label: &str| {
            let n = 200_000u64;
            let now = Instant::now();
            let mut sink = 0u64;
            let t0 = Instant::now();
            for i in 0..n {
                let t = now + Duration::from_micros(i);
                sink ^= app.presence_fp(wid, t);
                sink ^= app
                    .presence_overlay(wid, t)
                    .map_or(0, |g| u64::from(g.border_a));
                sink ^= app.presence_band_row(wid, 120, theme).len() as u64;
            }
            let el = t0.elapsed();
            eprintln!(
                "adv8 {label}: {:.1} ns/frame ({n} frames, sink={sink})",
                el.as_nanos() as f64 / n as f64
            );
        };
        bench(&mut app, "quiet");
        {
            let n = 200_000u64;
            let now = Instant::now();
            let mut sink = 0u64;
            let t0 = Instant::now();
            for i in 0..n {
                sink ^= app.presence_fp(wid, now + Duration::from_micros(i));
            }
            let fp_ns = t0.elapsed().as_nanos() as f64 / n as f64;
            let t0 = Instant::now();
            for i in 0..n {
                sink ^= app
                    .presence_overlay(wid, now + Duration::from_micros(i))
                    .map_or(0, |g| u64::from(g.border_a));
            }
            let ov_ns = t0.elapsed().as_nanos() as f64 / n as f64;
            let t0 = Instant::now();
            for _ in 0..n {
                sink ^= app.presence_band_row(wid, 120, theme).len() as u64;
            }
            let row_ns = t0.elapsed().as_nanos() as f64 / n as f64;
            let t0 = Instant::now();
            for _ in 0..n {
                sink ^= u64::from(crate::chrome_band::presence_tones(theme).drive[0]);
            }
            let tones_ns = t0.elapsed().as_nanos() as f64 / n as f64;
            eprintln!(
                "adv8 quiet split: fp={fp_ns:.1} overlay={ov_ns:.1} band_row={row_ns:.1} presence_tones()={tones_ns:.1} ns (sink={sink})"
            );
        }
        assert!(crate::fabric::apply_hold_for_test(
            &ctx,
            Some(crate::fabric::Hold {
                reason: "review".into(),
                origin: "local".into(),
            })
        ));
        app.on_presence_wake(&ctx.self_id, false);
        bench(&mut app, "hold row+rim");
        // The tick a row costs once a second: facts re-read + recompose.
        let t0 = Instant::now();
        let n = 2000u32;
        for i in 0..n {
            let _ = app.presence_tick(Instant::now() + Duration::from_secs(u64::from(i)));
        }
        eprintln!(
            "adv8 tick with row up: {:.1} us/tick",
            t0.elapsed().as_micros() as f64 / f64::from(n)
        );
    }

    /// ADV-9: `status level=` used to read the watermark of the window the
    /// session was FOCUSED in, else 0 — so a story the human read (folded)
    /// in a tab came back as `level=story` on `status` the moment another
    /// tab was in front, while that tab's chip (which reads the per-window
    /// watermark) was off. It reads the highest mark any window holds now.
    #[test]
    fn adv9_status_level_agrees_with_the_chip_for_a_background_tab() {
        crate::fabric::with_link_reset(
            adv9_status_level_agrees_with_the_chip_for_a_background_tab_body,
        );
    }

    fn adv9_status_level_agrees_with_the_chip_for_a_background_tab_body() {
        use crate::presence::StoryVerb;
        let (mut app, wid, sid_a, _ctx) = app_with_stub();
        let ia = app.windows[&wid].tab_set.tabs().len() - 1;
        assert_eq!(app.tell_story(sid_a, StoryVerb::Approval, ""), Ok(1));
        assert_eq!(app.presence_session_level(sid_a), Level::Story);
        // The human reads it: a key while calm folds the row.
        app.note_human_acted(wid);
        assert_eq!(app.presence_level(wid), Level::Quiet);
        assert_eq!(app.presence_session_level(sid_a), Level::Quiet);
        // Another tab comes to the front.
        let sid_b = app.next_session_id;
        app.push_stub_tab(wid, crate::stub_session(sid_b));
        assert_ne!(app.focused_session_id(wid), Some(sid_a));
        let chip = app
            .presence
            .slot(sid_a)
            .unwrap()
            .level(app.presence_view(wid).unwrap().watermark(sid_a))
            .chip();
        assert_eq!(chip, ChipLevel::Off, "the read story shows no dot");
        assert_eq!(
            app.presence_status_tail(sid_a),
            "hand=- level=quiet story=1",
            "the chip is off but `status` says: {}",
            app.presence_status_tail(sid_a)
        );
        let _ = ia;
    }
}
