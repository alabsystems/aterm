// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The App's side of presence ([`crate::presence`]): sensing each session's
//! facts from its own leaf locks, and hosting the engine driver. The sequencing
//! (fold facts into the per-session slot, project each window's focused session
//! onto its view, commit the row, the fold law, ripple and pulse, told stories,
//! deadline and tick) is `aterm_messages::presence::drive`; this file implements
//! its [`Desk`] on the App: the window walk, the facts, the on-screen test, the
//! motion policy, the re-grid, repaint, chips, herald and chime.
//!
//! Every entry point runs at change rate: lease, fabric and meta wakes, a
//! status-revision or agent-verdict move from the sweep, a tab switch, the
//! human's keystroke (the fold law), and the band's `since` tick while a row
//! is up. The frame path reads plain fields and allocates nothing
//! (`presence_overlay`, `presence_fp`).

use std::time::{Duration, Instant};

use aterm_core::terminal::RenderCell;
use aterm_messages::presence::drive::{self, Desk};
use aterm_render::Theme;
use aterm_session::SessionId;

use crate::app_render::OverlayGlow;
use crate::presence::{
    self, Facts, Hand, HoldFact, LeaseMark, Link, Slot, StoryVerb, TurnFact, WindowView,
};
use crate::{App, WindowId};

/// The chip level the indicator bits spell
/// (`aterm_messages::presence::chip_of_attention`).
pub(crate) use aterm_messages::presence::chip_of_attention;

/// The per-session presence state: the engine's table (the slots and each
/// `ttl=` supervisor claim's lapse, `drive::Table`) and this host's herald.
#[derive(Default, Debug)]
pub(crate) struct PresenceTable {
    table: drive::Table<presence::Native>,
    /// The menu-bar rows' and notifications' transition memory
    /// (`App::herald_session`), per session like the slots.
    pub(crate) herald: crate::status_item::Herald,
}

impl PresenceTable {
    pub(crate) fn slot(&self, session: u64) -> Option<&Slot> {
        self.table.slot(session)
    }

    pub(crate) fn retire(&mut self, session: u64) {
        self.table.retire(session);
        self.herald.retire(session);
    }
}

#[cfg(test)]
thread_local! {
    static PTY_RESIZES_FOR_PRESENCE: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// The chrome-row re-grids a [`App::with_chrome_regrid_batch`] recorded, to
/// apply at its end.
#[derive(Debug, Default)]
pub(crate) enum ChromeRegridBatch {
    /// None yet.
    #[default]
    Idle,
    /// These windows (`regrid_window_for_chrome_rows`).
    Windows(Vec<WindowId>),
    /// Every window (`regrid_for_chrome_rows`), which covers any one.
    All,
}

impl ChromeRegridBatch {
    fn window(&mut self, wid: WindowId) {
        match self {
            Self::Idle => *self = Self::Windows(vec![wid]),
            Self::Windows(windows) if !windows.contains(&wid) => windows.push(wid),
            Self::Windows(_) | Self::All => {}
        }
    }

    pub(crate) fn all(&mut self) {
        *self = Self::All;
    }

    /// Whether a re-grid of `wid` is recorded.
    fn covers(&self, wid: WindowId) -> bool {
        match self {
            Self::Idle => false,
            Self::Windows(windows) => windows.contains(&wid),
            Self::All => true,
        }
    }
}

#[cfg(test)]
thread_local! {
    static CHROME_REGRIDS_APPLIED: std::cell::RefCell<Vec<(WindowId, u16)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Log an applied chrome re-grid of `wid` with its chrome rows (test-only; a
/// headless window has no OS size to re-grid from).
#[cfg(test)]
pub(crate) fn note_chrome_regrid_applied(app: &App, wid: WindowId) {
    let rows = app.chrome_rows(wid);
    CHROME_REGRIDS_APPLIED.with(|log| log.borrow_mut().push((wid, rows)));
}

/// Take this thread's applied chrome re-grids, in order: `(window, chrome
/// rows)`.
#[cfg(test)]
pub(crate) fn take_chrome_regrids_applied() -> Vec<(WindowId, u16)> {
    CHROME_REGRIDS_APPLIED.with(|log| std::mem::take(&mut *log.borrow_mut()))
}

#[cfg(test)]
thread_local! {
    /// A test's presence clock: [`Desk::clock`] answers it while set, so a rig
    /// steps the driver's clock. `None`: the wall clock.
    static PRESENCE_CLOCK_FOR_TEST: std::cell::Cell<Option<Instant>> =
        const { std::cell::Cell::new(None) };
}

/// Set (or, with `None`, clear) this thread's presence clock for a test.
#[cfg(test)]
pub(crate) fn set_presence_clock_for_test(now: Option<Instant>) {
    PRESENCE_CLOCK_FOR_TEST.with(|c| c.set(now));
}

/// How many window re-grids the presence row has caused on this thread
/// (test-only).
#[cfg(test)]
pub(crate) fn presence_regrids() -> u64 {
    PTY_RESIZES_FOR_PRESENCE.with(|c| c.get())
}

/// Whether window `ws`'s grid re-grids when its chrome rows move: a real OS
/// window (a headless window keeps its inserted grid; its chrome caches are
/// the whole re-grid), or a test's stand-in for one. Only such a window has a
/// chrome regrow floor (ruling 414): a window whose grid never moves
/// for its chrome cannot flap, so none of its falls waits. It is also what
/// "glass" means to the two afford laws — the band's (`App::band_commit`)
/// and the presence row's ([`Desk::grid_rows`]) — so a stand-in whose rows a
/// test shrinks takes the window's own fall exactly as a real window does.
pub(crate) fn chrome_regrids_grid(ws: &crate::WindowState) -> bool {
    #[cfg(test)]
    if ws.chrome_regrids_for_test {
        return true;
    }
    ws.os_window.is_some()
}

/// A session's lease reduced to what the row's geometry depends on
/// ([`LeaseMark`]), shared by the sensed facts and [`Desk::lease_mark`] so they
/// compare like for like. An expired drive lease is `Free`.
fn lease_mark_of(lease: Option<&crate::Lease>, now_us: u64) -> LeaseMark {
    match lease {
        Some(crate::Lease::Turn { typing: true, .. }) => LeaseMark::Typing,
        Some(crate::Lease::Turn { typing: false, .. }) => LeaseMark::Settling,
        Some(crate::Lease::Drive { expires_us, .. }) if *expires_us > now_us => LeaseMark::Typing,
        _ => LeaseMark::Free,
    }
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

    /// Gather one session's facts: leaf locks only, each held for a copy; the
    /// agent reading is the status sweep's published one.
    fn presence_facts(&self, session: u64) -> Option<Facts> {
        let s = self.pool.get(session)?;
        let ctx = &s.ctx;
        let now_us = crate::metrics::now_us();
        let now = Instant::now();
        let (hand, lease_until, typing, mark) = {
            let lease = ctx.turn_lease.lock().unwrap_or_else(|p| p.into_inner());
            let typing = lease.as_ref().is_some_and(|l| l.driver_may_type(now_us));
            // The mark from this same read of the lease: the words and their
            // mark must be one moment of the lease (`drive::commit_rows`).
            let mark = lease_mark_of(lease.as_ref(), now_us);
            let (hand, lease_until) = match lease.as_ref() {
                Some(crate::Lease::Turn { id, driver, .. }) => (
                    Hand::DrivenTurn {
                        id: *id,
                        holder: driver.as_ref().map(|d| self.name_of_sid(d)),
                    },
                    None,
                ),
                Some(crate::Lease::Drive {
                    holder, expires_us, ..
                }) if *expires_us > now_us => (
                    Hand::DrivenLease {
                        holder: holder.clone(),
                    },
                    // The lapse posts no wake: it is a deadline instead.
                    Some(now + Duration::from_micros(expires_us - now_us)),
                ),
                _ => (Hand::None, None),
            };
            (hand, lease_until, typing, mark)
        };
        // Driving another session: an open turn on a peer whose lease names
        // this session as its driver.
        let hand = if matches!(hand, Hand::None) {
            self.driving_target(&ctx.self_id)
                .map_or(Hand::None, |dst| Hand::Driving {
                    sid: presence::short_sid(dst.as_str()),
                })
        } else {
            hand
        };
        let lease = match (&hand, mark) {
            (Hand::Driving { .. }, LeaseMark::Free) => LeaseMark::Driving,
            (_, mark) => mark,
        };
        let hold = ctx.fabric.hold().map(|h| HoldFact {
            reason: h.reason,
            fleet: h.origin == "fleet",
        });
        let mail = ctx.fabric.mail_facts();
        let (role, attention, attention_told_elsewhere) = {
            let meta = ctx.meta.lock().unwrap_or_else(|p| p.into_inner());
            // A stalled agent upgrade is told by the message band row and the
            // log. Its attention stays a fact (level, `status why=escalation`,
            // tab chip, rim); only the presence line's phase slot does not
            // repeat the band's words. Other owners' attention still reads there.
            let upgrade_only = meta.attention_owners.len() == 1
                && meta.attention_owners.effective_owner()
                    == Some(aterm_agent::harness::upgrade_drive::ATTENTION_OWNER);
            (
                meta.role.clone(),
                meta.attention.clone(),
                upgrade_only && meta.attention.is_some(),
            )
        };
        // The link as THIS App sees it (`fabric_state_seen`: the process
        // link, keyed away from a foreign test section in a test binary).
        let link = match crate::fabric::fabric_state_seen() {
            "connected" => Link::Connected {
                rtt_ms: crate::fabric::fabric_link_facts().1,
            },
            // The stall's age, not the last ack's: a long-quiet link is not
            // long stalled, and a bridge still dialing has no stall to date.
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
        // The sweep's published verdict (only for an identified agent, only
        // when its live zone changed); presence never re-reads the screen, so
        // band, rim, `status agent=` and `EVENT agent` agree.
        let (agent_seq, agent) = self.session_status.agent_reading(session);
        // The loop's next try on an API wall (`with_retry_plan`): the tab says
        // `→ 14:05 · 3m00s` beside the wall's words, which stay the screen's.
        let agent = match self.harness_waits.get(ctx.self_id.as_str()) {
            None => agent,
            Some(plan) => presence::with_retry_plan(
                agent,
                Some(plan),
                aterm_time::unix_secs().map_or(0, |s| i64::try_from(s).unwrap_or(0)),
                now,
            ),
        };
        // WHO ANSWERS THE WALL it is at: a wall aterm's own
        // harness — holding this session's claim — resumes by itself is a
        // record, not a stop; a wall nobody answers (no claim, another
        // holder's, the owner's switch off, a login, the memory banner) keeps
        // the stop. Read on every refresh, so a claim that arrives or lapses
        // moves it without a new agent reading.
        let wall_answer = match (agent.as_ref().map(|a| &a.phase), self.harness.as_ref()) {
            (Some(presence::AgentPhase::Wall { kind, reset, .. }), Some(host))
                if ctx
                    .meta
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .live_supervisor(now_us)
                    .is_some_and(|holder| holder == crate::harness_host::holder()) =>
            {
                host.wall_answer(*kind, reset.is_some())
            }
            _ => presence::WallAnswer::Nobody,
        };
        // The published input stall (`input_stall::InputWatches`): the fact
        // the frozen program's unmoving screen cannot carry — with the
        // explicit restart (`act`) its publication gave the watch's copy, the
        // one the attention entry and the menu row name too. No lock.
        let input_stall = self.session_status.input_stall(session).cloned();
        Some(Facts {
            role,
            attention,
            attention_told_elsewhere,
            shell,
            // The plan's changes count as news to the engine (see
            // `App::harness_wait_epoch`).
            agent_seq: agent_seq + self.harness_wait_epoch,
            agent,
            hand,
            lease_until,
            typing,
            lease,
            hold,
            mail,
            link,
            turn,
            input_stall,
            wall_answer,
        })
    }

    /// The name the band gives a driving session: its `meta role` when local
    /// and set, else its short sid. Who drives is the lease's record
    /// ([`crate::Lease::Turn::driver`]), never inferred from the edge table: a
    /// standing write edge is authority, not a hand, and an Owner-token turn
    /// has no driver.
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

    /// The session `src` is driving now: one whose open turn lease names `src`
    /// as its driver. Only a lease counts, not a standing edge.
    ///
    /// Each peer's `(local id, ctx)` is taken under the store guard, dropped
    /// before any lease lock; peers are walked in local-id order.
    /// `ctx.self_id` is the handle's sid (`register_session`).
    fn driving_target(&self, src: &SessionId) -> Option<SessionId> {
        let mut peers: Vec<_> = self
            .store
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .live_handles()
            .filter(|h| &h.sid != src)
            .map(|h| (h.local_id, std::sync::Arc::clone(&h.ctx)))
            .collect();
        peers.sort_by_key(|&(id, _)| id);
        peers.iter().find_map(|(_, ctx)| {
            let lease = ctx.turn_lease.lock().unwrap_or_else(|p| p.into_inner());
            matches!(
                lease.as_ref(),
                Some(crate::Lease::Turn { driver: Some(d), .. }) if d == src
            )
            .then(|| ctx.self_id.clone())
        })
    }

    /// A fact about `session` changed: re-read it, fold it in, and project it
    /// onto every window that shows the session (`drive::refresh_session`).
    /// `submitted` marks a turn's submit keypress — the ripple's edge.
    pub(crate) fn refresh_presence_session(&mut self, session: u64, submitted: bool) {
        drive::refresh_session(self, session, submitted);
    }

    /// The harness loop's next try for `sid` ([`crate::Wake::HarnessWait`]):
    /// stored, and the tab's words re-read where it changed.
    pub(crate) fn apply_harness_wait(&mut self, sid: &str, plan: Option<presence::RetryPlan>) {
        let changed = match plan {
            Some(plan) => {
                let was = self.harness_waits.insert(sid.to_string(), plan.clone());
                was.as_ref() != Some(&plan)
            }
            None => self.harness_waits.remove(sid).is_some(),
        };
        if !changed {
            return;
        }
        self.harness_wait_epoch += 1;
        if let Some(session) = self.local_session_of(&SessionId::new(sid)) {
            self.refresh_presence_session(session, false);
        }
    }

    /// The wake arms: resolve the fabric sid to the pool's local id.
    pub(crate) fn on_presence_wake(&mut self, sid: &SessionId, submitted: bool) {
        if let Some(session) = self.local_session_of(sid) {
            self.refresh_presence_session(session, submitted);
        }
    }

    /// Project the window's FOCUSED session onto its view and commit the row
    /// (`drive::refresh_window`).
    pub(crate) fn refresh_presence_window(&mut self, wid: WindowId) {
        drive::refresh_window(self, wid);
    }

    /// Every window: the tab-switch / focus-change / restore funnel.
    pub(crate) fn refresh_presence_all_windows(&mut self) {
        drive::refresh_all(self);
    }

    /// The fold law (`drive::human_acted`): the human acted in `wid`; a calm
    /// session's story is read and its row folds. The shipping key arm goes
    /// through [`Self::dispatch_hw_key`]; tests act through this.
    #[cfg(test)]
    pub(crate) fn note_human_acted(&mut self, wid: WindowId) {
        drive::human_acted(self, wid);
    }

    /// One key from the winit `KeyboardInput` arm: the echo-probe and strain
    /// brackets around [`Self::on_key`], and the fold law's read.
    ///
    /// The read follows `on_key`, since only it decides whether a key moves
    /// the front (a bound tab or pane switch): the session front before the key
    /// is read only if the key left it front, so a switch away and back does
    /// not fold and re-bear the row. The fold still lands before the next frame
    /// paints the key's echo; its `SIGWINCH` follows the key's bytes.
    pub(crate) fn dispatch_hw_key(&mut self, wid: WindowId, event: winit::event::KeyEvent) {
        let pressed = event.state == winit::event::ElementState::Pressed;
        let front = self.focused_session_id(wid);
        // Press-only, and never on focus alone.
        let seen = front.filter(|_| pressed);
        let echo = self.echo_probe_open(wid, front);
        // Strain (design §10.14): only a key dispatched from this arm counts
        // toward the strain engine's input→present slice; control
        // `send`/`feed`/`key` never do.
        if pressed {
            self.strain.note_hw_key(Instant::now());
        }
        self.hw_key_dispatch = true;
        self.on_key(wid, event);
        self.hw_key_dispatch = false;
        self.echo_probe_close(echo);
        if let Some(seen) = seen
            && self.focused_session_id(wid) == Some(seen)
        {
            self.note_human_acted_on(wid, seen);
        }
    }

    /// The fold law for a press (`drive::human_acted_on`): the human acted in
    /// `wid` while `session` was front, so that story is read even when the
    /// press brought another pane or tab front.
    pub(crate) fn note_human_acted_on(&mut self, wid: WindowId, session: u64) {
        drive::human_acted_on(self, wid, session);
    }

    /// Arm the presence timer at every pooled session's `ttl=` supervisor
    /// claim, at a successor's Commit. A lease a seamless update carried was
    /// seeded before its session was registered, so no `meta set` wake armed
    /// its lapse; without this a dead holder's claim would never lapse and this
    /// instance's supervisor host would never take the session.
    #[cfg(unix)]
    pub(crate) fn arm_supervisor_expiries(&mut self) {
        let sessions: Vec<u64> = self.pool.iter().map(|session| session.id).collect();
        for session in sessions {
            self.refresh_presence_session(session, false);
        }
    }

    /// The one instant the presence timer arms (`drive::deadline`): `None`
    /// on a quiet desktop.
    pub(crate) fn presence_deadline(&self, now: Instant) -> Option<Instant> {
        drive::deadline(self, now)
    }

    /// The timer's tick (`drive::tick`): the windows that need a repaint.
    pub(crate) fn presence_tick(&mut self, now: Instant) -> Vec<WindowId> {
        drive::tick(self, now)
    }

    /// The rim as an [`OverlayGlow`] for the frame path — plain field reads and
    /// arithmetic, no lock, no allocation; `None` on a quiet window.
    pub(crate) fn presence_overlay(&self, wid: WindowId, now: Instant) -> Option<OverlayGlow> {
        let ws = self.windows.get(&wid)?;
        // `View::rim_glow`: the quiet frame answers before the tones are
        // derived, so a rim-less window pays only the match. A running choice
        // pulse is the one ripple a rim-less window paints.
        let glow = ws.presence.rim_glow(now, self.presence_rim_on, || {
            crate::chrome_band::presence_tones(self.chrome_palette_theme())
        })?;
        Some(OverlayGlow {
            accent: pack(glow.accent),
            wash_a: glow.wash_a,
            border_a: glow.border_a,
            border_scale_q4: glow.border_scale_q4,
        })
    }

    /// The repaint-key term for `wid`: 0 on a quiet window.
    pub(crate) fn presence_fp(&self, wid: WindowId, now: Instant) -> u64 {
        self.windows.get(&wid).map_or(0, |ws| ws.presence.fp(now))
    }

    /// The band's painted row for this window at `cols`, cached on
    /// `(seed, cols, palette)` and borrowed (a hit clones nothing). Empty when
    /// no row is committed. The palette key is the row's inks plus the High
    /// Contrast latch, so an OS palette toggle repaints it.
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
            crate::chrome_band::band_colors(theme).hash(&mut h);
            let p = crate::chrome_band::presence_inks(theme);
            [p.drive, p.wait, p.stop, p.story].hash(&mut h);
            crate::chrome_band::forced_chrome().is_some().hash(&mut h);
            h.finish()
        };
        let key = (ws.presence.seed, cols, palette_key);
        if ws.presence_row_key != Some(key) {
            let row = match &ws.presence.words {
                Some(words) => crate::message_band::paint_presence_row(words, cols, theme),
                None => crate::message_band::blank_band_row(cols, theme),
            };
            if let Some(ws) = self.windows.get_mut(&wid) {
                ws.presence_row = row;
                ws.presence_row_key = Some(key);
            }
        }
        match self.windows.get(&wid) {
            Some(ws) => ws.presence_row.as_slice(),
            None => &[],
        }
    }

    /// Stamp each tab's presence chip level onto the strip metadata: the
    /// highest level across the tab's terminal leaves, folded over the
    /// indicator bit's own `Wait`.
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
                    // Each tab's story is read against this window's watermark
                    // for its session.
                    let watermark = ws.presence.watermark(session);
                    level = level.max(slot.chip(watermark));
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
            detail: Some(drive::report(&ws.presence, usize::from(ws.cols)).0),
            progress: None,
            busy: false,
            alarm: false,
            activates: false,
            bar_row: Some(0),
            capsules: Vec::new(),
        })
    }

    /// The band's words as the `chrome` verb reads them, for the tests
    /// ([`drive::report`]).
    #[cfg(test)]
    pub(crate) fn presence_report(&self, wid: WindowId) -> Option<(String, &'static str)> {
        let ws = self.windows.get(&wid)?;
        Some(drive::report(&ws.presence, usize::from(ws.cols)))
    }

    /// Re-grid one window after its chrome rows moved (per-window twin of
    /// `regrid_for_chrome_rows`): clears the strip cache and present key, and
    /// re-grids from its OS size (the one PTY resize).
    ///
    /// A resize entry point for the ledger (`site=chrome`, outermost, so
    /// `on_resize` books under it); only this shim is `#[track_caller]`.
    /// Inside [`Self::with_chrome_regrid_batch`] it is recorded and applied at
    /// the batch's end.
    #[cfg_attr(
        test,
        aterm_spec::refines(
            machine = "ChromeRegrowFloor",
            action = "PresRise",
            project = "aterm_gui::chrome_regrow_conformance::project"
        )
    )]
    #[cfg_attr(
        test,
        aterm_spec::refines(
            machine = "ChromeRegrowFloor",
            action = "PresFall",
            project = "aterm_gui::chrome_regrow_conformance::project"
        )
    )]
    #[cfg_attr(
        test,
        aterm_spec::refines(
            machine = "ChromeRegrowFloor",
            action = "PresTaken",
            project = "aterm_gui::chrome_regrow_conformance::project"
        )
    )]
    #[track_caller]
    pub(crate) fn regrid_window_for_chrome_rows(&mut self, wid: WindowId) {
        if let Some(batch) = self.chrome_regrid_batch.as_mut() {
            batch.window(wid);
            return;
        }
        let _site = crate::resize_ledger::SiteScope::enter(crate::resize_ledger::Cause::Chrome);
        self.regrid_window_for_chrome_rows_inner(wid);
    }

    /// Run `f` with every chrome-row re-grid it causes
    /// ([`Self::regrid_window_for_chrome_rows`], `regrid_for_chrome_rows`)
    /// recorded, then apply one re-grid per window from the rows the commits
    /// left.
    ///
    /// A freeze lift commits the presence fold and the band's growth back to
    /// back; unbatched that is two PTY resizes (N → N-1 → N), and a program that
    /// repaints only on a changed size never repaints. A re-grid to the rows the
    /// grid already has resizes nothing (`apply_term_resize`). Nested batches
    /// join the outer one.
    pub(crate) fn with_chrome_regrid_batch<R>(&mut self, f: impl FnOnce(&mut Self) -> R) -> R {
        if self.chrome_regrid_batch.is_some() {
            return f(self);
        }
        self.chrome_regrid_batch = Some(ChromeRegridBatch::default());
        // A panic in `f` must not leave the batch open: the batch ends and
        // applies what it recorded before the panic resumes.
        let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(self)));
        match self.chrome_regrid_batch.take().unwrap_or_default() {
            ChromeRegridBatch::Idle => {}
            ChromeRegridBatch::All => self.regrid_for_chrome_rows(),
            ChromeRegridBatch::Windows(windows) => {
                for wid in windows {
                    self.regrid_window_for_chrome_rows(wid);
                }
            }
        }
        out.unwrap_or_else(|panic| std::panic::resume_unwind(panic))
    }

    /// The terminal rows window `wid` measures a chrome commit against, and
    /// whether it has glass ([`chrome_regrids_grid`]). Normally `ws.rows`;
    /// inside a [`Self::with_chrome_regrid_batch`] that recorded `wid`, the rows
    /// the pending re-grid will give ([`Self::grid_dims_for`] over the OS size),
    /// so a batch's second commit is measured against the first's result.
    pub(crate) fn chrome_commit_grid_rows(&self, wid: WindowId) -> Option<(u16, bool)> {
        let ws = self.windows.get(&wid)?;
        // The OS size is read only inside a batch that recorded `wid`.
        let size = self
            .chrome_regrid_batch
            .as_ref()
            .is_some_and(|batch| batch.covers(wid))
            .then(|| ws.os_window.as_ref().map(|w| w.inner_size()))
            .flatten();
        Some((
            self.chrome_commit_grid_rows_at(wid, ws.rows, size),
            chrome_regrids_grid(ws),
        ))
    }

    /// The pure half of [`Self::chrome_commit_grid_rows`]: `rows`, or inside a
    /// batch that recorded `wid` with an OS `size`, the rows that re-grid gives.
    fn chrome_commit_grid_rows_at(
        &self,
        wid: WindowId,
        rows: u16,
        size: Option<winit::dpi::PhysicalSize<u32>>,
    ) -> u16 {
        match (&self.chrome_regrid_batch, size) {
            (Some(batch), Some(size)) if batch.covers(wid) => self.grid_dims_for(wid, size).0,
            _ => rows,
        }
    }

    /// Lift an update handoff's park (`Wake::UpdateHandoffFinished`): `finish`
    /// settles the attempt (clearing the park on failure or abort), then the
    /// presence rows and the band commit what they owe in one re-grid
    /// ([`Self::with_chrome_regrid_batch`]).
    ///
    /// The presence refresh runs after `finish`, since before it the park still
    /// holds the refresh. `finish` may close windows or quit; a re-grid
    /// recorded for a closed window is skipped at the batch's end.
    pub(crate) fn release_update_handoff_park(&mut self, finish: impl FnOnce(&mut Self)) {
        self.with_chrome_regrid_batch(|app| {
            finish(app);
            app.refresh_presence_all_windows();
            // Band row changes are deferred while a handoff is pending
            // (`sync_message_band_rows`); now it has settled, commit them.
            app.sync_messages();
        });
    }

    /// Lift a successor's freeze (the Commit wake, once
    /// `incoming_handoff_pending` is false): the presence rows and the band
    /// commit what each owes (rows born under the freeze, the landing's record,
    /// carried scrollback loss via [`Self::say_carried_history_loss`], the
    /// carried Settings drafts the restore could not put back via
    /// [`Self::say_carried_settings_drafts_loss`]) in one re-grid
    /// ([`Self::with_chrome_regrid_batch`]).
    #[cfg(unix)]
    pub(crate) fn release_successor_commit_freeze(&mut self) {
        self.with_chrome_regrid_batch(|app| {
            app.refresh_presence_all_windows();
            // Carried supervisor leases lapse on time if their holder does not
            // renew them here, so a supervisor host parked behind one takes the
            // session then.
            app.arm_supervisor_expiries();
            if let Some(build) = app.landing_at_commit.take() {
                app.retire_charging_surge();
                // …naming the tabs this successor adopted onto a blank
                // screen, if any.
                app.post_update_landed(
                    crate::build_info::version_display(),
                    build,
                    app.seamless_repainted_tabs(),
                );
            }
            // The scrollback the parent left behind: its row grows the band in
            // this one re-grid, not when the import settles later.
            app.say_carried_history_loss();
            // …and the carried Settings text the restore could not put back,
            // which the parent held until now (the review of round eight's U61).
            app.say_carried_settings_drafts_loss();
            app.sync_messages();
        });
    }

    /// Note a RISE of the chrome rows every window shares (a band row reached
    /// the glass, the tab strip grew) on every window's regrow floor
    /// (`aterm_messages::chrome::RegrowFloor::rose`, ruling 414).
    /// Noted on every window; read only where the grid re-grids for its
    /// chrome ([`chrome_regrids_grid`]).
    pub(crate) fn note_chrome_rise_everywhere(&mut self, now: Instant) {
        for ws in self.windows.values_mut() {
            ws.regrow_floor.rose(now);
        }
    }

    /// Window `wid`'s chrome regrow floor (`RegrowFloor::floor`): `None` for a
    /// window whose grid never re-grids for its chrome, or with no rise noted.
    pub(crate) fn chrome_regrow_floor(&self, wid: WindowId) -> Option<Instant> {
        self.windows
            .get(&wid)
            .filter(|ws| chrome_regrids_grid(ws))
            .and_then(|ws| ws.regrow_floor.floor())
    }

    /// The floor the band's quiet shrink waits: the latest of every window its
    /// re-grid resizes (`aterm_messages::chrome::latest`).
    pub(crate) fn band_regrow_floor(&self) -> Option<Instant> {
        aterm_messages::chrome::latest(
            self.windows
                .values()
                .filter(|ws| chrome_regrids_grid(ws))
                .map(|ws| ws.regrow_floor.floor()),
        )
    }

    /// The body of [`Self::regrid_window_for_chrome_rows`].
    fn regrid_window_for_chrome_rows_inner(&mut self, wid: WindowId) {
        #[cfg(test)]
        note_chrome_regrid_applied(self, wid);
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

    /// The window's presence level, the tests' probe.
    #[cfg(test)]
    pub(crate) fn presence_level(&self, wid: WindowId) -> presence::Level {
        self.windows
            .get(&wid)
            .map_or(presence::Level::Quiet, |ws| ws.presence.level)
    }

    /// The window's view, for the tests.
    #[cfg(test)]
    pub(crate) fn presence_view(&self, wid: WindowId) -> Option<&WindowView> {
        self.windows.get(&wid).map(|ws| &ws.presence)
    }

    /// `chrome`'s presence line for the front window (the frontmost, else
    /// the first): the engine's bytes ([`drive::chrome_line`]).
    pub(crate) fn presence_chrome_line(&self) -> String {
        let front = self
            .frontmost_window
            .or_else(|| self.windows.keys().next().copied());
        let cols = front
            .and_then(|w| self.windows.get(&w))
            .map_or(0, |ws| usize::from(ws.cols));
        drive::chrome_line(self, front, cols)
    }

    /// A session's level as `status level=` reports it, for the tests
    /// ([`drive::session_level`]).
    #[cfg(test)]
    pub(crate) fn presence_session_level(&self, session: u64) -> presence::Level {
        drive::session_level(self, session)
    }

    /// The additive presence `status` fields (`hand= level= story= why=`):
    /// the engine's bytes ([`drive::status_tail`]).
    pub(crate) fn presence_status_tail(&self, session: u64) -> String {
        drive::status_tail(self, session)
    }

    /// `aterm ctl story <verb> [<text>]` for `session` (`drive::tell`): the slot
    /// is brought current, the point noted, and every window showing the
    /// session re-projected; the phase slot shows the verb for
    /// [`presence::TOLD_FLASH`], and `chose` pulses and chimes. Returns the
    /// point's seq; `Err` if the pool no longer holds the session.
    pub(crate) fn tell_story(
        &mut self,
        session: u64,
        verb: StoryVerb,
        text: &str,
    ) -> Result<u64, &'static str> {
        drive::tell(self, session, verb, text).ok_or("no such session")
    }

    /// The choice chime: one quiet pip (the output voice's `Shimmer`) when the
    /// supervisor answers a question box by policy. Gate order matters, as in
    /// `on_bell`: serious mode, the Music effects master (`trail_sounds`; the
    /// chime is a synth voice), `choice_sound`, then the rate limiter last,
    /// because `try_fire` consumes its token. No focus test: the point is to
    /// hear a background session's answer. Headless and test audio is inert,
    /// and zero volume spends no token.
    pub(crate) fn chose_chime(&mut self, now: Instant) {
        let allowed = chose_chime_allowed(
            self.serious_mode_policy()
                .allows(crate::motion::SeriousEffect::TerminalSound),
            self.config.trail_sounds_or_default(),
            self.config.choice_sound_or_default(),
        );
        // A zero volume is a mute too, so it is read before the token as well.
        let gain = self.config.trail_sound_volume();
        if !allowed || gain <= 0.0 || !self.chose_chime_gate.try_fire(now) {
            return;
        }
        self.trail_audio
            .push(aterm_effects::trail_sound::SoundEvent {
                style: self.glow_style(),
                voice: self.config.trail_sound_voice(),
                kind: aterm_effects::trail_sound::SoundGesture::Output(
                    aterm_effects::trail_sound::OutputGesture::Shimmer,
                ),
                // Centred: a choice is about the session, not a column.
                pan: 0.0,
                heat: 0.0,
                hue: 0.0,
                gain,
                shifted: false,
                tone: aterm_effects::tone::Tone::Technical,
                // Punctuation, not weather: never feeds the ambient bed.
                bed: false,
            });
    }

    /// Forget a retired session's slot, and every window's watermark for it.
    pub(crate) fn retire_presence(&mut self, session: u64) {
        self.presence.retire(session);
        for ws in self.windows.values_mut() {
            ws.presence.watermarks.remove(&session);
        }
    }
}

/// The presence driver's host (`aterm_messages::presence::drive::Desk`):
/// every method a read of the App or one effect on it.
impl Desk for App {
    type V = presence::Native;
    type Win = WindowId;

    fn clock(&self) -> Instant {
        #[cfg(test)]
        if let Some(now) = PRESENCE_CLOCK_FOR_TEST.with(std::cell::Cell::get) {
            return now;
        }
        Instant::now()
    }

    fn table(&self) -> &drive::Table<presence::Native> {
        &self.presence.table
    }

    fn table_mut(&mut self) -> &mut drive::Table<presence::Native> {
        &mut self.presence.table
    }

    fn each_view(&self, f: &mut dyn FnMut(WindowId, &WindowView)) {
        for (wid, ws) in &self.windows {
            f(*wid, &ws.presence);
        }
    }

    fn view_of(&self, w: WindowId) -> Option<&WindowView> {
        self.windows.get(&w).map(|ws| &ws.presence)
    }

    fn view_of_mut(&mut self, w: WindowId) -> Option<&mut WindowView> {
        self.windows.get_mut(&w).map(|ws| &mut ws.presence)
    }

    fn front_session(&self, w: WindowId) -> Option<u64> {
        self.focused_session_id(w)
    }

    fn windows_showing(&self, session: u64) -> Vec<WindowId> {
        let mut windows = self.windows_with_focused_session(session);
        for (wid, _) in self.tabs_viewing_session(session) {
            if !windows.contains(&wid) {
                windows.push(wid);
            }
        }
        windows
    }

    fn holds_session(&self, session: u64) -> bool {
        self.pool.get(session).is_some()
    }

    fn sense(&self, session: u64) -> Option<Facts> {
        self.presence_facts(session)
    }

    /// The claim's lapse on the microsecond clock, mapped onto `now`'s: one
    /// millisecond past it, so the wake's lapse check sees it lapsed.
    fn supervisor_lapse(&self, session: u64, now: Instant) -> Option<Instant> {
        let at_us = self.pool.get(session).and_then(|s| {
            s.ctx
                .meta
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .supervisor_expiry()
        })?;
        let now_us = crate::metrics::now_us();
        Some(now + Duration::from_micros(at_us.saturating_sub(now_us)) + Duration::from_millis(1))
    }

    fn band_visible(&self, w: WindowId) -> bool {
        self.windows
            .get(&w)
            .map(Box::as_ref)
            .is_some_and(crate::messages_host::band_on_screen)
    }

    fn key_window(&self, w: WindowId) -> bool {
        self.windows.get(&w).is_some_and(|ws| ws.focused)
    }

    fn ripple_allowed(&self, w: WindowId, focused: bool) -> bool {
        let focused = self.motion_focus(w, focused);
        self.motion_policy(focused)
            .animate(crate::motion::MotionEffect::PresenceRipple)
            && self
                .serious_mode_policy()
                .allows(crate::motion::SeriousEffect::PresenceRipple)
    }

    fn band_toggle(&self) -> bool {
        self.presence_band_on()
    }

    fn rim_toggle(&self) -> bool {
        self.presence_rim_on()
    }

    /// Frozen mid-handoff for the same reason `sync_message_band_rows` is.
    fn rows_frozen(&self) -> bool {
        self.update_handoff_parked() || self.incoming_handoff_pending
    }

    /// Read from the live lease of every terminal session `w` hosts, in any
    /// tab (the window's re-grid resizes all of them).
    fn driver_may_type(&self, w: WindowId) -> bool {
        let Some(ws) = self.windows.get(&w) else {
            return false;
        };
        let now_us = crate::metrics::now_us();
        ws.tab_set.tabs().iter().any(|tab| {
            tab.root.any_leaf(&mut |view| {
                self.view_store
                    .get(*view)
                    .copied()
                    .and_then(crate::tab_model::View::terminal_session)
                    .and_then(|session| self.pool.get(session))
                    .is_some_and(|s| {
                        s.ctx
                            .turn_lease
                            .lock()
                            .unwrap_or_else(|p| p.into_inner())
                            .as_ref()
                            .is_some_and(|lease| lease.driver_may_type(now_us))
                    })
            })
        })
    }

    /// The LIVE mark, reduced as `presence_facts` reduces the read it senses:
    /// the session's own lease, else whether it drives a peer's turn.
    fn lease_mark(&self, session: u64) -> Option<LeaseMark> {
        let s = self.pool.get(session)?;
        let own = lease_mark_of(
            s.ctx
                .turn_lease
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .as_ref(),
            crate::metrics::now_us(),
        );
        Some(match own {
            LeaseMark::Free if self.driving_target(&s.ctx.self_id).is_some() => LeaseMark::Driving,
            own => own,
        })
    }

    /// The newest generation handed out ([`crate::presence::GenerationLook`])
    /// of every terminal session `w` hosts, in any tab, as an instant on the
    /// Desk's clock.
    fn looked_at(&self, w: WindowId) -> Option<Instant> {
        let ws = self.windows.get(&w)?;
        let mut last: Option<u64> = None;
        for tab in ws.tab_set.tabs() {
            tab.root.any_leaf(&mut |view| {
                let at = self
                    .view_store
                    .get(*view)
                    .copied()
                    .and_then(crate::tab_model::View::terminal_session)
                    .and_then(|session| self.pool.get(session))
                    .and_then(|s| s.ctx.generation_look.last_us());
                if let Some(at) = at {
                    last = Some(last.map_or(at, |x| x.max(at)));
                }
                false
            });
        }
        // Placed off the metrics clock's one anchor with no second clock read,
        // so `deadline` and the `tick` it arms see the same look.
        crate::metrics::instant_at_us(last?)
    }

    /// The window's terminal rows and whether it has glass
    /// ([`chrome_regrids_grid`]); inside a re-grid batch, the rows the pending
    /// re-grid gives ([`App::chrome_commit_grid_rows`]).
    fn grid_rows(&self, w: WindowId) -> Option<(u16, bool)> {
        self.chrome_commit_grid_rows(w)
    }

    /// Read off the window's stored floor (ruling 414), only where
    /// its grid re-grids for its chrome.
    fn chrome_floor(&self, w: WindowId) -> Option<Instant> {
        self.chrome_regrow_floor(w)
    }

    /// The row's birth, noted where the engine says so.
    fn chrome_rose(&mut self, w: WindowId, now: Instant) {
        if let Some(ws) = self.windows.get_mut(&w) {
            ws.regrow_floor.rose(now);
        }
    }

    fn herald_deadline(&self, now: Instant) -> Option<Instant> {
        match self.presence.herald.due(now) {
            (ready, _) if !ready.is_empty() => Some(now),
            (_, next) => next,
        }
    }

    /// The front window's hold, for the native menu's synchronous validate
    /// (the Fabric menu's halt pair reads it when the menu opens).
    fn projecting(&mut self, w: WindowId) {
        if self.frontmost_window == Some(w) {
            self.publish_front_hold(w);
        }
    }

    fn regrid(&mut self, w: WindowId) {
        if let Some(ws) = self.windows.get_mut(&w) {
            ws.presence_row_key = None;
        }
        #[cfg(test)]
        PTY_RESIZES_FOR_PRESENCE.with(|c| c.set(c.get() + 1));
        self.regrid_window_for_chrome_rows(w);
    }

    fn request_redraw(&self, w: WindowId) {
        if let Some(win) = self.windows.get(&w).and_then(|ws| ws.os_window.as_ref()) {
            win.request_redraw();
        }
    }

    fn restamp_chips(&mut self, windows: Vec<WindowId>) {
        self.refresh_tab_chrome_windows(windows);
    }

    /// The human channel dedups by transition itself, so a refresh that moved
    /// nothing costs two leaf-lock reads and no AppKit call.
    fn herald(&mut self, session: u64) {
        self.herald_session(session);
    }

    /// Every owed notification the rate limit now allows
    /// ([`crate::status_item::Herald::due`]).
    fn herald_owed(&mut self, now: Instant) {
        let (ready, _) = self.presence.herald.due(now);
        for session in ready {
            self.herald_session(session);
        }
    }

    /// Remove and record the lapsed claim (`meta-change field=supervisor
    /// value=-`, pushed as `EVENT meta`); the driver's re-read re-heralds the
    /// session, so a box the dead supervisor held reaches the menu row and the
    /// notification.
    fn lapse_supervisor(&mut self, session: u64) {
        let Some(ctx) = self.pool.get(session).map(|s| s.ctx.clone()) else {
            return;
        };
        if crate::session_timeline::lapse_supervisor(&ctx, crate::metrics::now_us())
            && self.subscribers.any()
        {
            self.subscribers
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .notify(session);
        }
    }

    fn forget(&mut self, session: u64) {
        self.presence.retire(session);
    }

    fn chime(&mut self, now: Instant) {
        self.chose_chime(now);
    }
}

/// Whether the choice chime may speak, before its rate limiter: serious mode
/// allows terminal sound, the Music effects master is on, and `choice_sound`
/// is on. Pure, so testable where the audio host is inert.
pub(crate) const fn chose_chime_allowed(
    serious_allows_sound: bool,
    music_effects: bool,
    choice_sound: bool,
) -> bool {
    serious_allows_sound && music_effects && choice_sound
}

fn pack(c: [u8; 3]) -> u32 {
    (u32::from(c[0]) << 16) | (u32::from(c[1]) << 8) | u32::from(c[2])
}

#[cfg(test)]
mod tests {
    //! The headless model, the fold law, the idle repaint invariant, the
    //! classifier gate, the golden cells, the chip level and the a11y sentence.
    //! The rim images are covered by `app_introspect`'s capture tests.

    use super::*;
    use crate::presence::{AgentPhase, AgentReading, ChipLevel, Level, Words, words_step};
    use crate::{App, WindowId};
    use aterm_messages::presence::FOLD_QUIET;
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

    /// The headless model (§7): a turn submit puts the rim on and the hand in
    /// the band before any status revision moves; a hold is the stop rim and
    /// `⊘ hold` within one wake; a delivery prints `✉1 task←✓x` with the trust
    /// glyph before the sender; a hold lands only on the session it names.
    #[test]
    fn a_turn_a_hold_and_a_delivery_reach_the_rim_and_the_band_at_change_rate() {
        // Reads the process-global link through `presence_level`, so it takes
        // the reset like every other reader.
        crate::fabric::with_link_reset(
            a_turn_a_hold_and_a_delivery_reach_the_rim_and_the_band_at_change_rate_body,
        );
    }

    fn a_turn_a_hold_and_a_delivery_reach_the_rim_and_the_band_at_change_rate_body() {
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
            typing: true,
        });
        app.on_presence_wake(&ctx.self_id, true);
        assert_eq!(app.presence_level(wid), Level::Driven);
        let (text, rim) = line(&app, wid);
        assert_eq!(rim, "drive");
        assert!(text.contains("\u{25c2} turn 41"), "{text}");
        assert_eq!(
            app.chrome_rows(wid),
            app.chrome_rows_shared(),
            "no re-grid under a driver that may still type"
        );
        // Its last Enter pressed: the row the hand asked for is born.
        assert!(crate::control::end_turn_input(
            &ctx.turn_lease,
            41,
            &ctx.self_id
        ));
        app.on_presence_wake(&ctx.self_id, false);
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

    /// The fold law (§7): a keypress while not calm is not a fold; calm without
    /// a keypress is not a fold; both together fold the row, with one PTY
    /// re-grid.
    #[test]
    fn the_row_folds_only_on_a_keypress_while_calm_and_pays_one_regrid() {
        // Reads the process-global link through `presence_level`, so it takes
        // the reset like every other reader.
        crate::fabric::with_link_reset(
            the_row_folds_only_on_a_keypress_while_calm_and_pays_one_regrid_body,
        );
    }

    fn the_row_folds_only_on_a_keypress_while_calm_and_pays_one_regrid_body() {
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

        // The key, while calm: the fold and exactly one re-grid. A key folds a
        // row only once it has stood `FOLD_QUIET`; the App's clock is the
        // wall's, so the birth is dated back.
        let born = &mut app.windows.get_mut(&wid).unwrap().presence.born_at;
        *born = born.and_then(|b| b.checked_sub(aterm_messages::presence::FOLD_QUIET));
        app.note_human_acted(wid);
        assert_eq!(app.presence_level(wid), Level::Quiet);
        assert_eq!(app.presence_view(wid).unwrap().rows, 0);
        assert_eq!(presence_regrids(), regrids + 2);
        assert_eq!(app.chrome_rows(wid), app.chrome_rows_shared());
        // …and a second key changes nothing.
        app.note_human_acted(wid);
        assert_eq!(presence_regrids(), regrids + 2);
    }

    /// A click lands where the person saw it: with a story row above the
    /// message band, a click on a band capsule presses that capsule, then reads
    /// the story (reading first would re-grid the band out from under the
    /// pointer). Control: the story still folds on that click.
    #[test]
    fn a_click_on_a_band_capsule_under_a_story_row_presses_the_capsule() {
        crate::fabric::with_link_reset(|| {
            let (mut app, wid, sid, ctx) = app_with_stub();
            assert!(crate::fabric::apply_hold_for_test(
                &ctx,
                Some(crate::fabric::Hold {
                    reason: "pause".into(),
                    origin: "local".into(),
                })
            ));
            app.on_presence_wake(&ctx.self_id, false);
            assert!(crate::fabric::apply_hold_for_test(&ctx, None));
            app.on_presence_wake(&ctx.self_id, false);
            assert_eq!(app.presence_level(wid), Level::Story);
            assert_eq!(app.presence_view(wid).unwrap().rows, 1);
            let id = app.post_message(
                aterm_messages::Message::new(
                    aterm_messages::tags::CONFIG,
                    aterm_messages::Severity::Warn,
                    "Misspelled setting",
                )
                .action(aterm_messages::Intent::OpenSettings {
                    route: "/packages".into(),
                }),
            );
            assert_eq!(app.message_band_rows, 1);
            let cols = usize::from(app.windows[&wid].cols);
            let capsule = app.band_presentation(cols).rows[0]
                .capsules
                .iter()
                .find(|c| !c.action.is_details())
                .expect("the authored capsule")
                .col
                + 1;
            let col = u16::try_from(capsule).unwrap();
            use crate::app_mouse::PointerAction;
            assert_eq!(
                app.pointer_cmd(PointerAction::MoveChrome { up: 1, col }),
                Ok(Some((-1, col)))
            );
            // Date the row born now, just before the press, so a slow setup
            // never ages it past `FOLD_QUIET`.
            app.windows.get_mut(&wid).unwrap().presence.born_at = Some(Instant::now());
            assert!(app.pointer_cmd(PointerAction::Click).is_ok());
            // The press is logged `Acted` (the navigation then folds the
            // row, design §10.5 H6).
            assert_eq!(
                app.messages
                    .log()
                    .get(id)
                    .and_then(|r| r.last_action.as_ref().map(|(label, _)| label.clone())),
                Some("Packages".to_string()),
                "the capsule the person clicked was pressed"
            );
            // The capsule opened Settings, so no session is front; the story
            // the person was looking at is still read.
            assert_eq!(app.focused_session_id(wid), None, "Settings is front");
            let seq = app.presence.slot(sid).unwrap().story_seq;
            assert!(seq > 0);
            assert_eq!(
                app.presence_view(wid).unwrap().watermark(sid),
                seq,
                "the story the person saw is read"
            );
            // A row born just now stands until born + `FOLD_QUIET`, where the
            // presence timer folds it with one re-grid, after the press.
            let young = app.presence_view(wid).unwrap();
            assert_eq!(young.rows, 1, "a young row waits out its minimum life");
            let due = young.born_at.expect("born") + FOLD_QUIET;
            assert_eq!(app.presence_deadline(Instant::now()), Some(due));
            let _ = app.presence_tick(due);
            assert_eq!(app.presence_view(wid).unwrap().rows, 0);
        });
    }

    /// A press that moves the front reads the story the person saw, through the
    /// event loop's own sequence (`mouse_input_then_fold`): a click into the
    /// other split pane reads the previously front pane's story and leaves the
    /// newly focused pane's row up, unread.
    #[test]
    fn a_click_into_another_pane_reads_the_story_seen_and_not_the_panes() {
        crate::fabric::with_link_reset(|| {
            use crate::app_mouse::PointerAction;
            use winit::event::{ElementState, MouseButton};
            let mut app = App::headless_for_test();
            let wid = WindowId(0);
            let top = app.focused_session_id(wid).expect("top terminal");
            let bottom = app.split_active_stub_tab_dir(wid, crate::pane::SplitDir::Horizontal);
            assert_eq!(app.focused_session_id(wid), Some(bottom));
            let seq_top = app.tell_story(top, StoryVerb::Approval, "").expect("held");
            let seq_bottom = app
                .tell_story(bottom, StoryVerb::Approval, "")
                .expect("held");
            assert_eq!(app.presence_level(wid), Level::Story);
            assert_eq!(app.presence_view(wid).unwrap().rows, 1);
            // The pointer over the top pane, then the arm's press and release.
            assert!(
                app.pointer_cmd(PointerAction::Move { row: 1, col: 2 })
                    .is_ok()
            );
            app.mouse_input_then_fold(wid, ElementState::Pressed, MouseButton::Left);
            app.mouse_input_then_fold(wid, ElementState::Released, MouseButton::Left);
            assert_eq!(
                app.focused_session_id(wid),
                Some(top),
                "the press focused it"
            );
            let view = app.presence_view(wid).unwrap();
            assert_eq!(view.watermark(bottom), seq_bottom, "the story seen is read");
            assert_eq!(view.watermark(top), 0, "the pane the press focused is not");
            assert_eq!(app.presence_level(wid), Level::Story, "its row is up");
            assert_eq!(app.presence_view(wid).unwrap().rows, 1);
            // The next act in the pane now in front reads it. A read folds a
            // row only once it has stood `FOLD_QUIET`, so the birth is dated
            // back.
            let born = &mut app.windows.get_mut(&wid).unwrap().presence.born_at;
            *born = born.and_then(|b| b.checked_sub(FOLD_QUIET));
            app.mouse_input_then_fold(wid, ElementState::Pressed, MouseButton::Left);
            assert_eq!(app.presence_view(wid).unwrap().watermark(top), seq_top);
            assert_eq!(app.presence_view(wid).unwrap().rows, 0);
            app.mouse_input_then_fold(wid, ElementState::Released, MouseButton::Left);
        });
    }

    /// THE FOLD QUIET (ruling 394), on the real App: a settling
    /// turn's lease born and dropped inside a millisecond — the incident's
    /// schedule (measured 2026-09-28: 121x52 -> 51 -> 52 within ~0.8 ms, an
    /// alt-screen Claude Code left stale) — pays ONE PTY re-grid, the birth,
    /// not two; a second lease inside the quiet pays none; the row stands
    /// blank with an empty `chrome` band meanwhile; and the timer the
    /// presence deadline arms folds it once, one re-grid, after the quiet.
    #[test]
    fn a_quick_show_and_fold_pays_one_regrid_not_two() {
        // Reads the process-global link through `presence_level`, so it takes
        // the reset like every other reader.
        crate::fabric::with_link_reset(a_quick_show_and_fold_pays_one_regrid_not_two_body);
    }

    fn a_quick_show_and_fold_pays_one_regrid_not_two_body() {
        let (mut app, wid, _sid, ctx) = app_with_stub();
        let regrids = presence_regrids();
        let settling = || {
            Some(crate::Lease::Turn {
                id: 7,
                driver: None,
                typing: false,
            })
        };
        *ctx.turn_lease.lock().unwrap() = settling();
        app.on_presence_wake(&ctx.self_id, false);
        assert_eq!(app.presence_view(wid).unwrap().rows, 1);
        assert_eq!(presence_regrids(), regrids + 1, "the birth: one re-grid");
        *ctx.turn_lease.lock().unwrap() = None;
        app.on_presence_wake(&ctx.self_id, false);
        assert_eq!(app.presence_level(wid), Level::Quiet);
        assert_eq!(
            app.presence_view(wid).unwrap().rows,
            1,
            "the fold waits out the quiet"
        );
        assert_eq!(presence_regrids(), regrids + 1, "not two re-grids");
        assert_eq!(line(&app, wid).0, "", "a blank row, and the wire says so");
        assert_eq!(app.chrome_rows(wid), app.chrome_rows_shared() + 1);
        // The lease comes back inside the quiet: the fold is cancelled.
        *ctx.turn_lease.lock().unwrap() = settling();
        app.on_presence_wake(&ctx.self_id, false);
        assert_eq!(presence_regrids(), regrids + 1, "a flap pays nothing");
        *ctx.turn_lease.lock().unwrap() = None;
        app.on_presence_wake(&ctx.self_id, false);
        assert_eq!(presence_regrids(), regrids + 1);
        // The presence timer: armed at the fold's due instant, and its tick
        // there folds the row once.
        let now = Instant::now();
        let due = app
            .presence_deadline(now)
            .expect("the held fold arms the timer");
        assert!(due > now && due <= now + FOLD_QUIET, "{due:?}");
        let _ = app.presence_tick(due);
        assert_eq!(app.presence_view(wid).unwrap().rows, 0);
        assert_eq!(presence_regrids(), regrids + 2, "the fold: one re-grid");
        assert_eq!(app.chrome_rows(wid), app.chrome_rows_shared());
        assert_eq!(app.presence_deadline(due), None, "a quiet desk again");
        let _ = app.presence_tick(due + FOLD_QUIET);
        assert_eq!(presence_regrids(), regrids + 2, "folded once");
    }

    /// The tab retry plan, through the window: the sweep publishes an API wall
    /// for the front session; the next try told over `Wake::HarnessWait` lands
    /// on that tab's words (`→ <clock> · <left>` on the band, `next try` in the
    /// sentence), and clearing it or a past plan removes it. Controls: the
    /// wall's band word stays the screen's, and another session's plan changes
    /// nothing here.
    #[test]
    fn the_loops_next_try_reaches_the_tab_beside_an_api_wall_and_only_there() {
        crate::fabric::with_link_reset(|| {
            let (mut app, _wid, sid, ctx) = app_with_stub();
            let now = Instant::now();
            let rows = aterm_phase::prompt::fixtures::screen(
                aterm_phase::prompt::fixtures::API_ERROR_TALL_PANE_MEASURED,
            );
            app.session_status.agent_observe(
                sid,
                crate::control::ScreenGen { epoch: 0, seq: 1 },
                Some(rows),
                crate::presence::Cursor::Unknown,
                Some("claude".into()),
                42,
                false,
                now,
            );
            app.refresh_presence_session(sid, false);
            let mine = ctx.self_id.as_str().to_string();
            let bare = app.presence_chrome_line();
            assert!(bare.contains("can't reach the API"), "{bare}");
            assert!(
                !bare.contains("next try") && !bare.contains('\u{2192}'),
                "{bare}"
            );

            let unix = aterm_time::unix_secs().map_or(0, |s| i64::try_from(s).unwrap_or(0));
            app.apply_harness_wait(&mine, Some(plan(unix + 180)));
            let told = app.presence_chrome_line();
            assert!(told.contains("can't reach the API"), "{told}");
            assert!(told.contains('\u{2192}'), "the band has the try: {told}");
            assert!(told.contains("next try"), "the sentence says it: {told}");

            // Cleared, or already past: the row is as the screen alone made it.
            app.apply_harness_wait(&mine, None);
            assert_eq!(app.presence_chrome_line(), bare);
            app.apply_harness_wait(&mine, Some(plan(unix - 60)));
            assert_eq!(app.presence_chrome_line(), bare, "a past try shows nothing");
            // A plan for another session is not this tab's.
            app.apply_harness_wait("s-not-this-one", Some(plan(unix + 180)));
            assert_eq!(app.presence_chrome_line(), bare);
        });
    }

    /// A retry plan for a try at unix second `at`, its clock read in UTC.
    fn plan(at: i64) -> presence::RetryPlan {
        presence::RetryPlan::with_offset(at, Some(0))
    }

    /// The seconds a countdown clause prints (`fmt_dur`'s `59s` and `2m59s`
    /// shapes) — the figure the row's last `since` clause carries.
    fn printed_left(words: &Words) -> u64 {
        let clause = words.since.last().expect("a countdown clause");
        let (minutes, seconds) = clause.split_once('m').unwrap_or(("0", clause));
        minutes.parse::<u64>().expect("minutes") * 60
            + seconds
                .trim_end_matches('s')
                .parse::<u64>()
                .expect("seconds")
    }

    /// The tab's retry countdown ticks: with a plan beside an API wall, the
    /// presence timer is armed at the words' 1 s clock, each tick recomposes
    /// and dirties the window, and `<left>` falls by the elapsed second, in both
    /// the `chrome` verb's bytes and the painted row. A plan whose time comes
    /// lapses on the same clock. Controls: a cleared or past plan drops to the
    /// minute cadence (no repaint); a headless window arms no text tick, and
    /// only a change of plan moves its words.
    #[test]
    fn the_tabs_retry_countdown_ticks_on_the_presence_timer_while_a_plan_stands() {
        /// Puts the test's presence clock back to the wall's, panic or not.
        struct ClockReset;
        impl Drop for ClockReset {
            fn drop(&mut self) {
                set_presence_clock_for_test(None);
            }
        }
        crate::fabric::with_link_reset(|| {
            let _reset = ClockReset;
            let (mut app, wid, sid, ctx) = app_with_stub();
            let mine = ctx.self_id.as_str().to_string();
            let t0 = Instant::now();
            set_presence_clock_for_test(Some(t0));
            let rows = aterm_phase::prompt::fixtures::screen(
                aterm_phase::prompt::fixtures::API_ERROR_TALL_PANE_MEASURED,
            );
            app.session_status.agent_observe(
                sid,
                crate::control::ScreenGen { epoch: 0, seq: 1 },
                Some(rows),
                crate::presence::Cursor::Unknown,
                Some("claude".into()),
                42,
                false,
                t0,
            );
            app.refresh_presence_session(sid, false);
            let unix = aterm_time::unix_secs().map_or(0, |s| i64::try_from(s).unwrap_or(0));
            let words = |app: &App| {
                app.presence_view(wid)
                    .and_then(|v| v.words.clone())
                    .expect("a row is up")
            };
            // The clock the words follow, stepped like the loop's timer does:
            // to the armed deadline, never past it, never sleeping.
            let tick_to = |app: &mut App, t: Instant| {
                set_presence_clock_for_test(Some(t));
                app.presence_tick(t)
            };

            // Headless control: no glass, no text clock. The plan reaches the
            // words at once, and nothing counts it down.
            app.apply_harness_wait(&mine, Some(plan(unix + 180)));
            let told = words(&app);
            assert!(
                told.since.iter().any(|c| c.starts_with('\u{2192}')),
                "the plan is on the row: {:?}",
                told.since
            );
            assert_eq!(
                app.presence_deadline(t0),
                None,
                "a band nobody can see arms no text tick, plan or no plan"
            );

            // A window with glass: the row's own clock is armed at 1 s.
            app.windows.get_mut(&wid).unwrap().band_on_screen_for_test = true;
            let mut t = t0;
            let due = app
                .presence_deadline(t)
                .expect("a wall with a plan arms the presence timer");
            assert!(
                due <= t + Duration::from_secs(1),
                "the countdown prints seconds, so the tick is at +1 s, not +{:?}",
                due.saturating_duration_since(t)
            );
            let first = printed_left(&words(&app));
            assert!(
                (178..=180).contains(&first),
                "the plan is three minutes ahead: {:?}",
                words(&app).since
            );
            let clock = words(&app).since[0].clone();
            let mut left = first;
            let mut keys = vec![app.presence_fp(wid, t)];
            for step in 1..=8u64 {
                let due = app
                    .presence_deadline(t)
                    .unwrap_or_else(|| panic!("tick {step}: the timer went unarmed at {left}s"));
                assert!(due > t, "tick {step}: armed in the past");
                assert!(
                    due <= t + Duration::from_secs(1),
                    "tick {step}: armed +{:?} out",
                    due.saturating_duration_since(t)
                );
                t = due;
                let dirty = tick_to(&mut app, t);
                assert_eq!(dirty, vec![wid], "tick {step}: the countdown repaints");
                let now_left = printed_left(&words(&app));
                assert_eq!(now_left, left - 1, "tick {step}: one second, one figure");
                left = now_left;
                let w = words(&app);
                assert_eq!(w.since[0], clock, "the try's clock time stands still");
                let printed = w.since.last().unwrap().clone();
                let chrome = app.presence_chrome_line();
                assert!(
                    chrome.contains(&format!("{clock} \u{b7} {printed}")),
                    "tick {step}: `chrome` prints {clock} · {printed}: {chrome}"
                );
                keys.push(app.presence_fp(wid, t));
            }
            assert!(
                keys.windows(2).all(|p| p[0] != p[1]),
                "every tick moves the frame's repaint key: {keys:?}"
            );
            assert_eq!(
                first - left,
                8,
                "eight seconds of timer, eight seconds off the figure"
            );
            // The painted row carries the same figure the words do.
            assert_eq!(
                app.presence_view(wid).unwrap().rows,
                1,
                "the row is committed"
            );
            let painted = text_of(app.presence_band_row(wid, 120, aterm_render::Theme::default()));
            assert!(
                painted.contains(&format!("{clock} \u{b7} {}", words(&app).since[1])),
                "the band paints the figure it ticked to: `{painted}`"
            );

            // The plan lapses with its time: a loop that never speaks again
            // (parked, restarting, asleep) would otherwise leave the past try
            // on the tab. The presence clock alone is stepped past the try.
            let lapsed = t0 + Duration::from_secs(181);
            assert_eq!(
                tick_to(&mut app, lapsed),
                vec![wid],
                "the lapse repaints the row"
            );
            assert!(
                words(&app).since.is_empty(),
                "a try whose time has come is not printed: {:?}",
                words(&app).since
            );
            let chrome = app.presence_chrome_line();
            assert!(chrome.contains("can't reach the API"), "{chrome}");
            assert!(
                !chrome.contains('\u{2192}') && !chrome.contains("next try"),
                "the cause alone: {chrome}"
            );
            t = lapsed;

            // Control: the plan cleared. The row is the screen's alone; one
            // last tick of the old cadence may run, then the timer is a minute
            // out and repaints nothing.
            app.apply_harness_wait(&mine, None);
            assert!(
                words(&app).since.is_empty(),
                "the wall alone prints no figure: {:?}",
                words(&app).since
            );
            if let Some(due) = app.presence_deadline(t) {
                t = due.max(t);
                let _ = tick_to(&mut app, t);
            }
            let minute_due = app
                .presence_deadline(t)
                .expect("a visible row keeps its minute clock");
            assert_eq!(
                minute_due,
                t + Duration::from_secs(60),
                "no per-second tick without a plan"
            );
            for s in 1..=30u64 {
                assert!(
                    tick_to(&mut app, t + Duration::from_secs(s)).is_empty(),
                    "+{s}s: a bare wall repaints nothing on a timer"
                );
            }
            t += Duration::from_secs(30);

            // CONTROL: a plan already past is a try made, not a countdown.
            app.apply_harness_wait(&mine, Some(plan(unix - 60)));
            assert!(words(&app).since.is_empty(), "{:?}", words(&app).since);
            assert_eq!(
                app.presence_deadline(t),
                Some(minute_due),
                "a past plan arms no seconds clock: the minute clock stands"
            );

            // A new plan arms it again. Its countdown is laid off the wall
            // clock while the ticks above stepped the presence clock 211 s
            // ahead, so the plan must be far enough ahead not to have lapsed.
            app.apply_harness_wait(&mine, Some(plan(unix + 400)));
            let due = app.presence_deadline(t).expect("a fresh plan re-arms it");
            assert_eq!(due, t + Duration::from_secs(1), "{:?}", words(&app).since);
            let before = printed_left(&words(&app));
            assert_eq!(tick_to(&mut app, due), vec![wid]);
            assert_eq!(
                printed_left(&words(&app)),
                before - 1,
                "the new plan counts down too"
            );
        });
    }

    /// The repaint invariant (§7): on a quiet window `presence_fp` is 0 across
    /// a thousand frames, the overlay is `None`, and the frame path allocates
    /// nothing (no cache capacity, plain fields, `Copy` reads).
    /// `aterm ctl story` reaches the band within one revision (phase slot
    /// `✓ approved`, `status` `level=story story=<n>`, `chrome` the same
    /// words), and the slot returns to the phase after three seconds. A
    /// session the pool does not hold is an error.
    #[test]
    fn a_told_story_reaches_the_band_and_the_verbs_within_one_revision() {
        // Reads the process-global link, so it takes the reset like every
        // other reader.
        crate::fabric::with_link_reset(|| {
            use crate::presence::{StoryVerb, TOLD_FLASH};
            let (mut app, wid, sid, _ctx) = app_with_stub();
            let revision = app.session_status.revision(sid);
            let record = app.session_status_record(sid).expect("live");
            // The round-19 tail, with round 22's screen stamp appended after it.
            assert!(
                record.contains(" hand=- level=quiet story=0 why=- path=live program="),
                "{record}"
            );
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
            assert!(
                record.contains(" hand=- level=story story=1 why=- path=live program="),
                "{record}"
            );
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
            assert!(
                record.contains(" level=story story=2 why=- path=live program="),
                "{record}"
            );
            assert_eq!(
                app.tell_story(9999, StoryVerb::Timeout, ""),
                Err("no such session")
            );

            // The hand rides `status` too: a peer's open turn.
            *_ctx.turn_lease.lock().unwrap() = Some(crate::Lease::Turn {
                id: 41,
                driver: None,
                typing: true,
            });
            app.on_presence_wake(&_ctx.self_id, false);
            let record = app.session_status_record(sid).expect("live");
            assert!(
                record.contains(" hand=turn:41 level=driven story=2 why=- path=live program="),
                "{record}"
            );
            let chrome = app.presence_chrome_line();
            assert!(
                chrome.starts_with("presence rim=drive level=driven band=\""),
                "{chrome}"
            );
        });
    }

    #[test]
    fn presence_fp_is_zero_over_a_thousand_idle_frames() {
        let (mut app, wid, sid, _ctx) = app_with_stub();
        app.refresh_presence_session(sid, false);
        app.refresh_presence_all_windows();
        let now = Instant::now();
        let cap_before = app.windows[&wid].presence_row.capacity();
        for i in 0..1000u64 {
            let t = now + Duration::from_millis(i);
            assert_eq!(app.presence_fp(wid, t), 0);
            assert!(app.presence_overlay(wid, t).is_none());
            assert!(app.presence_tick(t).is_empty(), "no window is dirtied");
            assert!(app.presence_deadline(t).is_none(), "no timer is armed");
        }
        assert_eq!(app.windows[&wid].presence_row.capacity(), cap_before);
        let v = app.presence_view(wid).unwrap();
        assert_eq!(cap_before, 0, "a quiet window never painted a row");
        assert_eq!(v.rows, 0);
        assert!(v.ripple_at.is_none());
    }

    /// The classifier gate (§7), presence's half: a refresh folds the sweep's
    /// published reading and never classifies; a thousand refreshes and a
    /// status revision move cost zero `aterm_phase` calls. The sweep's half is
    /// pinned in `session_status`.
    #[test]
    fn a_presence_refresh_never_classifies() {
        use crate::session_status::{ActivitySample, Evidence};
        let (mut app, _wid, sid, _ctx) = app_with_stub();
        let calls = presence::classifier_calls();
        for _ in 0..1000 {
            app.refresh_presence_session(sid, false);
        }
        let t0 = Instant::now();
        let ev = Evidence {
            shell: None,
            completed_block: None,
            lifecycle: None,
            foreground_job: Some(true),
            activity: ActivitySample {
                alt_screen: false,
                content_seq: 1,
                last_input: None,
                last_output: Some(t0),
            },
        };
        let before = app.session_status.revision(sid);
        app.session_status.observe(sid, &ev, t0);
        app.session_status
            .observe(sid, &ev, t0 + Duration::from_millis(800));
        assert_ne!(
            app.session_status.revision(sid),
            before,
            "the revision moved"
        );
        for _ in 0..100 {
            app.refresh_presence_session(sid, false);
        }
        assert_eq!(
            presence::classifier_calls() - calls,
            0,
            "presence folds the published verdict; it never reads the screen"
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
    /// story row is 126 cells, so the whole line is read at 160 columns.
    fn rows(now: Instant) -> Vec<(&'static str, Slot, &'static str)> {
        let role = || Some("worker:claude-satcomp".to_string());
        let mut out = vec![
            (
                "driven",
                slot(
                    Facts {
                        role: role(),
                        agent_seq: 1,
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
                "worker:claude-satcomp  busy 3m12s  \u{25c2} manager \u{00b7} turn 41  \u{2709}2 task\u{2190}\u{2713}manager  ctx 41% left  \u{27df} 12ms",
            ),
            (
                "driving",
                slot(
                    Facts {
                        role: Some("agent:claude-manager".into()),
                        agent_seq: 1,
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
                "agent:claude-manager  busy 8s  \u{25b8} @s-1e91  \u{2709}1 report\u{2190}\u{2713}worker  ctx 77% left  \u{27df} 5ms",
            ),
            (
                "prompt",
                slot(
                    Facts {
                        role: role(),
                        agent_seq: 1,
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
                "worker:claude-satcomp  bash approval 0s  \u{25c2} manager  ctx 38% left  \u{27df} 4ms",
            ),
            (
                "question",
                slot(
                    Facts {
                        role: role(),
                        agent_seq: 1,
                        agent: agent(AgentPhase::Question, Some(12)),
                        link: Link::Connected { rtt_ms: Some(6) },
                        ..Facts::default()
                    },
                    now - Duration::from_secs(120),
                ),
                "worker:claude-satcomp  question 2m00s  \u{2014}  ctx 12% left \u{26a0}  \u{27df} 6ms",
            ),
            (
                "limited",
                slot(
                    Facts {
                        role: role(),
                        agent_seq: 1,
                        agent: agent(
                            AgentPhase::Wall {
                                kind: aterm_phase::WallKind::UsageSession,
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
                "worker:claude-satcomp  limited \u{2192} 19:30 \u{00b7} 1d 22h  \u{2014}  \u{2709}3 task\u{2190}\u{2713}manager  ctx 9% left \u{26a0}  \u{27df} 5ms",
            ),
            (
                "hold-fleet-lost",
                slot(
                    Facts {
                        role: role(),
                        agent_seq: 1,
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
                "worker:claude-satcomp  idle 40s  \u{2298} hold fabric-lost \u{00b7}fleet\u{1f512}  \u{2709}1 \u{2191}2  ctx 41% left  \u{2715} lost",
            ),
            (
                "attention",
                slot(
                    Facts {
                        role: role(),
                        attention: Some("needs a decision".into()),
                        agent_seq: 1,
                        agent: agent(AgentPhase::Busy, Some(50)),
                        link: Link::Connected { rtt_ms: Some(3) },
                        ..Facts::default()
                    },
                    now,
                ),
                "worker:claude-satcomp  needs a decision  \u{2014}  ctx 50% left  \u{27df} 3ms",
            ),
            (
                "stalled",
                slot(
                    Facts {
                        role: role(),
                        agent_seq: 1,
                        agent: agent(AgentPhase::Busy, Some(91)),
                        link: Link::Stalled {
                            age_ms: Some(7_400),
                        },
                        ..Facts::default()
                    },
                    now - Duration::from_secs(120),
                ),
                "worker:claude-satcomp  busy 2m00s  \u{2014}  ctx 91% left  ~ 7s",
            ),
        ];
        // The story row: three settled turns, then a hold that lifted.
        let mut s = Slot::new(now - Duration::from_secs(130));
        let base = now - Duration::from_secs(130);
        for id in 1..=3 {
            s.absorb(
                Facts {
                    role: role(),
                    agent_seq: 1,
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
            agent_seq: 1,
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
            "worker:claude-satcomp  \u{25c7} quiet since 2m09s \u{00b7} 3 turns \u{00b7} 1 mail \u{00b7} held 1m00s, resumed 1m00s ago  \u{2014}  \u{2709}1 note\u{2190}\u{2713}h-x  ctx 41% left  \u{27df} 12ms",
        ));
        out
    }

    /// The golden cells (§7): every band row at 120, 60 and 24 columns, in
    /// light, dark and each stock Windows High-Contrast palette. The 120-col
    /// line is literal; at every width the painted text is the fitted words
    /// (one margin cell each side), every cell sits on the band, every ink
    /// clears it, emoji-capable glyphs use text presentation, and hand + phase
    /// + mail survive to 24 columns.
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
                // Mail survives where there is any; with none there is no
                // slot to survive (ruling 366).
                assert_eq!(
                    fitted.contains('\u{2709}'),
                    !w.mail.is_empty(),
                    "{name} at {cols}: mail survives: {fitted}"
                );
                let hand_glyph = w.hand.chars().next().unwrap();
                assert!(
                    fitted.contains(hand_glyph),
                    "{name} at {cols}: hand survives: {fitted}"
                );
                for (palette, hc, theme) in &palettes {
                    let paint = || crate::message_band::paint_presence_row(&w, cols, *theme);
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

    /// The chip is a level: the indicator bit spells `Wait`, and the presence
    /// stamp folds the session's level over it (hold: filled diamond, story:
    /// dot, quiet: the bit alone).
    #[test]
    fn the_chip_level_folds_the_session_level_over_the_indicator_bit() {
        // Reads the process-global link, so it takes the reset like every
        // other reader.
        crate::fabric::with_link_reset(|| {
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
            let held = ChipLevel::Stop(crate::presence::StopCause::Hold);
            assert_eq!(metadata(&app)[front].attention, held);
            assert_eq!(held.chrome_states(), &["attention", "stop"]);
            assert_eq!(held.help(), Some("Held"));
            assert!(crate::fabric::apply_hold_for_test(&ctx, None));
            app.refresh_presence_session(sid, false);
            assert_eq!(app.presence_level(wid), Level::Story);
            assert_eq!(metadata(&app)[front].attention, ChipLevel::Story);
            app.note_human_acted(wid);
            assert_eq!(metadata(&app)[front].attention, ChipLevel::Off);
            assert_eq!(chip_of_attention(true), ChipLevel::Wait);
            assert!(ChipLevel::Off < ChipLevel::Story && ChipLevel::Story < ChipLevel::Wait);
            assert!(ChipLevel::Wait < ChipLevel::Stop(crate::presence::StopCause::Wall));
            assert!(
                ChipLevel::Stop(crate::presence::StopCause::Wall) < held,
                "a tab's max over its panes keeps a hold"
            );
        });
    }

    /// The a11y round trip (§7), App side: the band's message is the sixth
    /// chrome message, carries the spoken sentence, and sits on bar row 0 only
    /// while the row is committed.
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
            // The turn is settling, so its presence row may be committed.
            typing: false,
        });
        app.on_presence_wake(&ctx.self_id, false);
        let m = app.presence_a11y_message(wid).expect("the band speaks");
        assert_eq!(m.message, ChromeMessage::PresenceStatus);
        assert!(m.text.starts_with("driven, turn 41"), "{}", m.text);
        assert_eq!(m.bar_row, Some(0));
        assert!(!m.activates);
        assert!(ChromeMessage::ORDER.contains(&ChromeMessage::PresenceStatus));
    }

    // ---- honesty-cost review cases ----------------------------------------

    fn driven_words(now: Instant) -> presence::Words {
        let then = now - Duration::from_secs(192);
        let mut s = Slot::new(then);
        s.absorb(
            Facts {
                role: Some("worker:claude-satcomp".into()),
                agent_seq: 1,
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
        // The link is process-global, and a sibling's bridge turns `Quiet`
        // into `Note`, so every test that reads it takes the reset.
        crate::fabric::with_link_reset(
            review_r1_focus_alone_never_reads_another_sessions_story_body,
        );
    }

    fn review_r1_focus_alone_never_reads_another_sessions_story_body() {
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

    /// A bound tab switch away and back pays no re-grid: tab A wants the row
    /// (a turn lease), tab B none. `next_tab` then `prev_tab` through
    /// [`App::dispatch_hw_key`] must not read the pending fold as the person's
    /// first key in B. Control: a key B's PTY takes still folds the row.
    #[test]
    fn a_bound_tab_switch_away_and_back_pays_no_regrid() {
        crate::fabric::with_link_reset(a_bound_tab_switch_away_and_back_pays_no_regrid_body);
    }

    fn a_bound_tab_switch_away_and_back_pays_no_regrid_body() {
        use winit::event::{ElementState, KeyEvent};
        use winit::keyboard::{Key, KeyCode, KeyLocation, ModifiersState, PhysicalKey, SmolStr};
        let key = |ch: &str, code: KeyCode| {
            KeyEvent::synthetic_for_test(
                PhysicalKey::Code(code),
                Key::Character(SmolStr::new(ch)),
                Some(SmolStr::new(ch)),
                KeyLocation::Standard,
                ElementState::Pressed,
                false,
            )
        };
        let (mut app, wid, sid_a, ctx_a) = app_with_stub();
        app.frontmost_window = Some(wid);
        app.keybindings = crate::keybinding::Keybindings::resolved(Some(
            &[
                ("ctrl+y".to_string(), "next_tab".to_string()),
                ("ctrl+u".to_string(), "prev_tab".to_string()),
            ]
            .into_iter()
            .collect(),
        ));
        let ia = app.windows[&wid].tab_set.tabs().len() - 1;
        let sid_b = app.next_session_id;
        app.push_stub_tab(wid, crate::stub_session(sid_b));
        app.switch_tab_in(wid, ia);
        assert_eq!(app.focused_session_id(wid), Some(sid_a));
        let regrids = presence_regrids();
        *ctx_a.turn_lease.lock().unwrap() = Some(crate::Lease::Turn {
            id: 7,
            driver: None,
            typing: false,
        });
        app.on_presence_wake(&ctx_a.self_id, false);
        assert_eq!(app.presence_view(wid).unwrap().rows, 1, "A's row is born");
        assert_eq!(presence_regrids(), regrids + 1, "the birth");
        // The row has long stood its minimum life (ruling 394); the App's
        // clock is the wall's, so the birth is dated back to match.
        let born = &mut app.windows.get_mut(&wid).unwrap().presence.born_at;
        *born = born.and_then(|b| b.checked_sub(FOLD_QUIET + FOLD_QUIET));
        app.windows.get_mut(&wid).unwrap().mods = ModifiersState::CONTROL;
        app.dispatch_hw_key(wid, key("y", KeyCode::KeyY));
        assert_eq!(app.focused_session_id(wid), Some(sid_b), "next_tab ran");
        assert_eq!(app.presence_view(wid).unwrap().rows, 1, "pending, blank");
        assert_eq!(presence_regrids(), regrids + 1, "no re-grid on the way out");
        app.dispatch_hw_key(wid, key("u", KeyCode::KeyU));
        assert_eq!(app.focused_session_id(wid), Some(sid_a), "prev_tab ran");
        assert_eq!(app.presence_view(wid).unwrap().rows, 1);
        assert_eq!(app.presence_view(wid).unwrap().fold_since, None);
        assert_eq!(
            presence_regrids(),
            regrids + 1,
            "a bound switch away and back is no geometry change at all"
        );
        // CONTROL: away again, and a key B's PTY takes folds the row.
        app.dispatch_hw_key(wid, key("y", KeyCode::KeyY));
        assert_eq!(app.focused_session_id(wid), Some(sid_b));
        app.windows.get_mut(&wid).unwrap().mods = ModifiersState::empty();
        app.dispatch_hw_key(wid, key("x", KeyCode::KeyX));
        assert_eq!(app.focused_session_id(wid), Some(sid_b));
        assert_eq!(app.presence_view(wid).unwrap().rows, 0, "the key folds it");
        assert_eq!(presence_regrids(), regrids + 2, "the birth and the fold");
    }

    /// A click into a quiet pane and back pays no re-grid: pane A wants the row
    /// (a turn lease), pane B none. The mouse arm reads the session front
    /// before its press (`mouse_input_then_fold`); the blank row is only the
    /// front's to fold. Control: a click in B with B front still folds it.
    #[test]
    fn a_click_into_a_quiet_pane_and_back_pays_no_regrid() {
        crate::fabric::with_link_reset(a_click_into_a_quiet_pane_and_back_pays_no_regrid_body);
    }

    fn a_click_into_a_quiet_pane_and_back_pays_no_regrid_body() {
        use crate::app_mouse::PointerAction;
        use winit::event::{ElementState, MouseButton};
        let wid = WindowId(0);
        let click = |app: &mut App, row: u16| {
            assert!(app.pointer_cmd(PointerAction::Move { row, col: 2 }).is_ok());
            app.mouse_input_then_fold(wid, ElementState::Pressed, MouseButton::Left);
            app.mouse_input_then_fold(wid, ElementState::Released, MouseButton::Left);
        };
        // The last grid row the pointer can reach: inside the bottom pane.
        let last_row = |app: &mut App| {
            (0..200u16)
                .rev()
                .find(|&row| app.pointer_cmd(PointerAction::Move { row, col: 2 }).is_ok())
                .expect("a grid row")
        };
        let mut app = App::headless_for_test();
        app.frontmost_window = Some(wid);
        let top = app.focused_session_id(wid).expect("top terminal");
        let ctx_top = app.pool.get(top).expect("pooled").ctx.clone();
        let bottom = app.split_active_stub_tab_dir(wid, crate::pane::SplitDir::Horizontal);
        assert_eq!(app.focused_session_id(wid), Some(bottom));
        click(&mut app, 1);
        assert_eq!(app.focused_session_id(wid), Some(top), "A is front");
        let regrids = presence_regrids();
        *ctx_top.turn_lease.lock().unwrap() = Some(crate::Lease::Turn {
            id: 7,
            driver: None,
            typing: false,
        });
        app.on_presence_wake(&ctx_top.self_id, false);
        assert_eq!(app.presence_view(wid).unwrap().rows, 1, "A's row is born");
        assert_eq!(presence_regrids(), regrids + 1, "the birth");
        // The row has long stood its minimum life (ruling 394); the App's
        // clock is the wall's, so the birth is dated back to match.
        let born = &mut app.windows.get_mut(&wid).unwrap().presence.born_at;
        *born = born.and_then(|b| b.checked_sub(FOLD_QUIET + FOLD_QUIET));
        let row_b = last_row(&mut app);
        click(&mut app, row_b);
        assert_eq!(app.focused_session_id(wid), Some(bottom), "the press moved");
        assert_eq!(app.presence_view(wid).unwrap().rows, 1, "pending, blank");
        assert_eq!(
            presence_regrids(),
            regrids + 1,
            "a click away reads A, and A's read folds no row B shows"
        );
        click(&mut app, 1);
        assert_eq!(app.focused_session_id(wid), Some(top), "back in A");
        assert_eq!(app.presence_view(wid).unwrap().rows, 1);
        assert_eq!(app.presence_view(wid).unwrap().fold_since, None);
        assert_eq!(
            presence_regrids(),
            regrids + 1,
            "a click away and back is no geometry change at all"
        );
        // CONTROL: away again, and a click in B (B front) folds the row.
        let row_b = last_row(&mut app);
        click(&mut app, row_b);
        assert_eq!(app.focused_session_id(wid), Some(bottom));
        assert_eq!(presence_regrids(), regrids + 1);
        click(&mut app, row_b);
        assert_eq!(app.focused_session_id(wid), Some(bottom));
        assert_eq!(
            app.presence_view(wid).unwrap().rows,
            0,
            "the click folds it"
        );
        assert_eq!(presence_regrids(), regrids + 2, "the birth and the fold");
    }

    /// R1b: the same root — a story the human READ does not come back after
    /// they read a second tab.
    #[test]
    fn review_r1b_a_story_already_read_does_not_return_on_a_tab_switch() {
        // Reads the process-global link through `presence_level`, so it takes
        // the reset like every other reader.
        crate::fabric::with_link_reset(
            review_r1b_a_story_already_read_does_not_return_on_a_tab_switch_body,
        );
    }

    fn review_r1b_a_story_already_read_does_not_return_on_a_tab_switch_body() {
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

    /// A cooperative lease that lapses (driver crashed) posts no wake; the
    /// model arms its expiry as a deadline and the tick re-reads the hand, so
    /// the rim and `◂ holder` lift when `lease status` says `none`.
    #[test]
    fn review_r2_a_lapsed_drive_lease_lifts_the_rim() {
        // Reads the process-global link, so it takes the reset like every
        // other reader.
        crate::fabric::with_link_reset(|| {
            let (mut app, wid, sid, ctx) = app_with_stub();
            let now_us = crate::metrics::now_us();
            *ctx.turn_lease.lock().unwrap() = Some(crate::Lease::Drive {
                holder: "manager".into(),
                expires_us: now_us + 20_000,
                conn: None,
                hard: false,
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
                ("none", "hand=- level=quiet story=0 why=-".to_string()),
                "`lease status` says none but the window says rim={rim} band=`{text}`"
            );
            assert_eq!(app.presence_view(wid).unwrap().rows, 0, "the row folded");
            assert!(
                app.presence_deadline(Instant::now()).is_none(),
                "nothing re-arms on a quiet window"
            );
        });
    }

    /// A turn that times out drops the lease while the worker is still busy;
    /// the live phase keeps the slot and the story says the timeout, never
    /// `◇ quiet` over a running worker.
    #[test]
    fn review_r3_a_timed_out_turn_leaves_a_busy_worker_reading_busy() {
        let now = Instant::now();
        let mut s = Slot::new(now);
        s.absorb(
            Facts {
                role: Some("worker:claude-satcomp".into()),
                agent_seq: 1,
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
                agent_seq: 1,
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
        assert_eq!(
            w.sentence,
            "busy, 1 turn, 1 timed out, context 40 percent left"
        );
        // The same story on an IDLE worker is the quiet row.
        s.absorb(
            Facts {
                role: Some("worker:claude-satcomp".into()),
                agent_seq: 2,
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
            "quiet, since 3s, 1 turn, 1 timed out, context 40 percent left"
        );
    }

    /// R4: an unread TASK still waits after a later note lands — the wait
    /// rim stays, and the mail slot names the task, not the note.
    #[test]
    fn review_r4_an_unread_task_still_waits_after_a_later_note() {
        // Reads the process-global link, so it takes the reset like every
        // other reader.
        crate::fabric::with_link_reset(|| {
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
        });
    }

    /// A `since` that prints seconds (`3m12s` included) ticks every second.
    /// Which clauses print seconds is proved in the engine's tests.
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
            ws.band_on_screen_for_test = true;
            ws.presence.words_due = Some(now + words_step(ws.presence.words.as_ref().unwrap()));
        }
        let d = app.presence_deadline(now).expect("a row is up");
        assert!(
            d <= now + Duration::from_secs(1),
            "`busy 3m12s` changes at +1 s but the next tick is at +{:?}",
            d.saturating_duration_since(now)
        );
    }

    /// The words keep their own clock: another owner's wake before they are
    /// due recomposes nothing, and an invisible band (occluded, minimized,
    /// headless) arms no text tick.
    #[test]
    fn the_since_tick_keeps_its_own_clock_and_skips_a_hidden_band() {
        let (mut app, wid, _sid, _ctx) = app_with_stub();
        let now = Instant::now();
        let w = driven_words(now);
        {
            let ws = app.windows.get_mut(&wid).unwrap();
            ws.presence.words = Some(w.clone());
            ws.presence.rows = 1;
            ws.band_on_screen_for_test = true;
        }
        assert_eq!(
            app.presence_deadline(now),
            Some(now),
            "a row not yet ticked is due at once"
        );
        app.windows.get_mut(&wid).unwrap().presence.words_due = Some(now + Duration::from_secs(1));
        // A wake at 30 fps (the band's motion) before the figure moves.
        for ms in [33, 66, 99] {
            assert!(
                app.presence_tick(now + Duration::from_millis(ms))
                    .is_empty()
            );
        }
        let ws = app.windows.get(&wid).unwrap();
        assert_eq!(ws.presence.words.as_ref(), Some(&w), "nothing recomposed");
        assert_eq!(ws.presence.words_due, Some(now + Duration::from_secs(1)));
        // Hidden: the text tick is no wake.
        app.windows.get_mut(&wid).unwrap().occluded = true;
        assert_eq!(
            app.presence_deadline(now),
            None,
            "a hidden band arms nothing"
        );
        app.windows.get_mut(&wid).unwrap().occluded = false;
        assert_eq!(
            app.presence_deadline(now),
            Some(now + Duration::from_secs(1)),
            "revealed, it arms again"
        );
    }

    /// Words that change outside the tick (a wake, a tab switch) re-arm their
    /// clock: a minutes row's +60 s due must not freeze a seconds row that
    /// replaced it.
    #[test]
    fn new_words_from_a_wake_rearm_their_own_clock() {
        let (mut app, wid, sid, _ctx) = app_with_stub();
        let then = Instant::now() - Duration::from_secs(192);
        let _ = app.presence.table.absorb(
            sid,
            Facts {
                role: Some("worker:claude-satcomp".into()),
                agent_seq: 1,
                agent: agent(AgentPhase::Busy, Some(41)),
                hand: Hand::DrivenTurn {
                    id: 41,
                    holder: Some("manager".into()),
                },
                ..Facts::default()
            },
            then,
        );
        let now = Instant::now();
        let minutes = presence::Words {
            since: vec!["2h05m".into()],
            ..presence::Words::default()
        };
        assert_eq!(words_step(&minutes), Duration::from_secs(60));
        {
            let ws = app.windows.get_mut(&wid).unwrap();
            ws.presence.words = Some(minutes);
            ws.presence.rows = 1;
            ws.band_on_screen_for_test = true;
            ws.presence.words_due = Some(now + Duration::from_secs(60));
        }
        app.refresh_presence_window(wid);
        let ws = app.windows.get(&wid).unwrap();
        let words = ws.presence.words.as_ref().expect("a busy row");
        assert!(
            words_step(words) == Duration::from_secs(1),
            "the wake's words print seconds: {:?}",
            words.since
        );
        let due = ws.presence.words_due.expect("armed");
        assert!(
            due <= Instant::now() + Duration::from_secs(1),
            "the seconds row is due within a second, not at the minutes row's +{:?}",
            due.saturating_duration_since(now)
        );
        let d = app.presence_deadline(now).expect("a row is up");
        assert!(d <= Instant::now() + Duration::from_secs(1));
    }

    /// The limited figure counts down to the reset (design §1) with the
    /// design's separator, and prints nothing when the reset cannot be placed
    /// on the clock.
    #[test]
    fn review_r6_the_limited_figure_counts_down_to_the_reset() {
        let now = Instant::now();
        let mut s = Slot::new(now);
        s.absorb(
            Facts {
                role: Some("worker:claude-satcomp".into()),
                agent_seq: 1,
                agent: agent(
                    AgentPhase::Wall {
                        kind: aterm_phase::WallKind::UsageSession,
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
        assert_eq!(a.sentence, "limited, resets 19:30, context 9 percent left");
        // Unplaceable: the reset alone, no figure.
        let mut u = Slot::new(now);
        u.absorb(
            Facts {
                agent_seq: 1,
                agent: agent(
                    AgentPhase::Wall {
                        kind: aterm_phase::WallKind::UsageSession,
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
                agent_seq: 1,
                agent: agent(
                    AgentPhase::Wall {
                        kind: aterm_phase::WallKind::UsageSession,
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
        // Reads the process-global link, so it takes the reset like every
        // other reader.
        crate::fabric::with_link_reset(|| {
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
            let painted = crate::message_band::paint_presence_row(
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
        });
    }

    /// A turn is attributed only to the session the dispatch resolved as its
    /// driver (`Lease::Turn::driver`): not a read-screen observer, and not a
    /// standing write edge.
    #[test]
    fn review_r8_a_turn_is_not_attributed_to_a_read_only_observer() {
        // Reads the process-global link, so it takes the reset like every
        // other reader.
        crate::fabric::with_link_reset(|| {
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
                typing: true,
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
                typing: true,
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
        });
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
        let cached = app.windows[&wid].presence_row.as_ptr();
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
                agent_seq: 1,
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
        let row = crate::message_band::paint_presence_row(&w, 120, aterm_render::Theme::default());
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

    /// The link's recovery reaches the fabric slot: the tick re-reads the facts
    /// of a session with a row up (a headless App has no proxy for the bridge
    /// lane's wake). A waiting note keeps the row up across the stall.
    #[test]
    fn review_r11_a_recovered_link_reaches_the_fabric_slot() {
        // The link is process-global: attaching a bridge outside the reset
        // section would disturb sibling tests.
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
        // The belt runs at the words' own clock, for a band on screen.
        app.windows.get_mut(&wid).unwrap().band_on_screen_for_test = true;
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

    /// The role slot cannot spell a hold, a hand or a mail count: the band's
    /// glyphs are stripped from free text and runs of spaces collapse to one.
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
                agent_seq: 1,
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
        // Reads the process-global link, so it takes the reset like every
        // other reader.
        crate::fabric::with_link_reset(|| {
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
        });
    }

    /// A turn ledger carried across a self-update handoff (`handoff_carry.rs`,
    /// `carried=1`) describes a turn settled in the previous process: the slot's
    /// first sight of it is the baseline (`TurnFact::carried`), not news. A
    /// turn settled in this process is still news.
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

    /// The fabric slot's `~ <age>s` is the age of the stall: a bridge still
    /// dialing prints `~` with no figure (`fabric_stalled_ms()` is `None`), and
    /// a stall is dated from its own transition, not the last ack.
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

    /// After a `link down`, the age is the time since the last `link up`
    /// report: a link up for two seconds reads `~ 2s` the moment it stalls.
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
            // The link stays up for 2.1 s: its ack is aged under the link lock
            // rather than waited out.
            crate::fabric::backdate_link_ack(Duration::from_millis(2100));
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

    /// The lease names its driver from the dispatch's scope: an Owner-token
    /// turn names nobody, an edge-scoped turn names the edge's source, and the
    /// driver's band says `▸ @<sid>` for exactly that turn. A standing write
    /// edge never makes a session the named driver.
    #[test]
    fn adv3_an_owner_turn_is_not_credited_to_a_standing_write_edge() {
        // Reads the process-global link, so it takes the reset like every
        // other reader.
        crate::fabric::with_link_reset(|| {
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
            // A hang detector, never a latency budget.
            let wait_for_lease = |ctx: &crate::SessionCtx| -> u64 {
                aterm_spec::wait::until_within(
                    aterm_spec::wait::MUST_HAPPEN,
                    "the turn to hold the lease",
                    || match ctx.turn_lease.lock().unwrap().as_ref() {
                        Some(crate::Lease::Turn { id, .. }) => Some(*id),
                        _ => None,
                    },
                )
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
        });
    }

    /// The hand slot's text is painted in `presence_inks` (the presence hues
    /// at the 4.5:1 text floor), not `presence_tones` (the 3:1 non-text floor
    /// the rim keeps).
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
                let row = crate::message_band::paint_presence_row(&w, 120, theme);
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

    /// The idle invariant with the worker's status revisions replayed: a
    /// Running/Quiet oscillation on a quiet session leaves `presence_fp` at 0
    /// and paints no row.
    #[test]
    fn adv5_presence_fp_stays_zero_while_status_revisions_replay() {
        use crate::session_status::{ActivitySample, Evidence};
        let (mut app, wid, sid, _ctx) = app_with_stub();
        crate::fabric::with_link_reset(|| {
            app.refresh_presence_session(sid, false);
            app.refresh_presence_all_windows();
            let t0 = Instant::now();
            let mut ev = Evidence {
                shell: None,
                completed_block: None,
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
            assert_eq!(app.windows[&wid].presence_row.capacity(), 0);
            eprintln!("adv5: {revisions} revisions replayed, fp stayed 0");
        });
    }

    /// `status level=` reads the highest watermark any window holds, so a story
    /// read in one tab does not come back as `level=story` when another tab is
    /// front.
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
            .chip(app.presence_view(wid).unwrap().watermark(sid_a));
        assert_eq!(chip, ChipLevel::Off, "the read story shows no dot");
        assert_eq!(
            app.presence_status_tail(sid_a),
            "hand=- level=quiet story=1 why=-",
            "the chip is off but `status` says: {}",
            app.presence_status_tail(sid_a)
        );
        let _ = ia;
    }

    /// The chime's gate is a conjunction: serious mode allowing sound, the Music
    /// effects master, and `choice_sound` — any one off is silence.
    #[test]
    fn the_choice_chime_speaks_only_when_every_gate_is_open() {
        for serious in [false, true] {
            for music in [false, true] {
                for choice in [false, true] {
                    assert_eq!(
                        super::chose_chime_allowed(serious, music, choice),
                        serious && music && choice,
                        "serious={serious} music={music} choice={choice}"
                    );
                }
            }
        }
    }

    /// `ctl story chose <policy>` plays one chime (the output voice's
    /// `Shimmer`, centred, at synth volume, never feeding the bed); a second
    /// choice inside the two-second floor plays none; a non-choice story plays
    /// nothing.
    #[test]
    fn a_chose_story_plays_one_shimmer_chime_under_its_rate_limit() {
        crate::fabric::with_link_reset(
            a_chose_story_plays_one_shimmer_chime_under_its_rate_limit_body,
        );
    }

    fn a_chose_story_plays_one_shimmer_chime_under_its_rate_limit_body() {
        use aterm_effects::trail_sound::{OutputGesture, SoundGesture};
        let (mut app, _wid, sid, _ctx) = app_with_stub();
        app.trail_audio = crate::trail_audio::TrailAudio::capturing_for_test();
        assert!(app.tell_story(sid, StoryVerb::Approval, "").is_ok());
        assert!(
            app.trail_audio.take_captured_for_test().is_empty(),
            "an approval is no choice: no chime"
        );
        assert!(app.tell_story(sid, StoryVerb::Chose, "recommended").is_ok());
        let pushed = app.trail_audio.take_captured_for_test();
        assert_eq!(pushed.len(), 1, "one chime");
        let chime = &pushed[0];
        assert!(
            matches!(chime.kind, SoundGesture::Output(OutputGesture::Shimmer)),
            "the whisper-level output pip"
        );
        assert!(!chime.bed && chime.pan == 0.0);
        assert!(
            (chime.gain - app.config.trail_sound_volume()).abs() < f32::EPSILON,
            "scaled by the synth volume"
        );
        assert!(app.tell_story(sid, StoryVerb::Chose, "recommended").is_ok());
        assert!(
            app.trail_audio.take_captured_for_test().is_empty(),
            "a second choice inside the floor is not a cascade"
        );
    }

    /// A muted chime never spends the limiter's token: with `choice_sound`
    /// off, the Music master off, zero volume, or serious mode, a `chose` story
    /// plays nothing and leaves the token for the next chime.
    #[test]
    fn a_muted_choice_chime_does_not_spend_its_rate_limit() {
        crate::fabric::with_link_reset(a_muted_choice_chime_does_not_spend_its_rate_limit_body);
    }

    fn a_muted_choice_chime_does_not_spend_its_rate_limit_body() {
        for mute in 0..4 {
            let (mut app, _wid, sid, _ctx) = app_with_stub();
            app.trail_audio = crate::trail_audio::TrailAudio::capturing_for_test();
            match mute {
                0 => app.config.choice_sound = Some(false),
                1 => app.config.trail_sounds = Some(false),
                2 => app.config.trail_sound_volume = Some(0.0),
                _ => app.serious_mode = true,
            }
            assert!(app.tell_story(sid, StoryVerb::Chose, "recommended").is_ok());
            assert!(
                app.trail_audio.take_captured_for_test().is_empty(),
                "muted ({mute}) plays nothing"
            );
            assert!(
                app.chose_chime_gate.try_fire(Instant::now()),
                "the muted chime ({mute}) left its token unspent"
            );
        }
    }

    /// `ctl story chose <policy>`: the band tells it with its own glyph, the
    /// spoken sentence names the harness, the quiet summary counts it apart
    /// from approvals, and any pulse it starts is a choice pulse.
    #[test]
    fn a_chose_story_is_told_by_the_harness_with_its_own_glyph() {
        crate::fabric::with_link_reset(
            a_chose_story_is_told_by_the_harness_with_its_own_glyph_body,
        );
    }

    fn a_chose_story_is_told_by_the_harness_with_its_own_glyph_body() {
        let (mut app, wid, sid, _ctx) = app_with_stub();
        assert_eq!(app.tell_story(sid, StoryVerb::Chose, "recommended"), Ok(1));
        let (text, rim) = line(&app, wid);
        assert!(text.contains("\u{25c6} chose"), "{text}");
        assert!(text.contains("recommended"), "{text}");
        assert_eq!(rim, "none", "a story has no rim");
        let chrome = app.presence_chrome_line();
        assert!(
            chrome.ends_with(" sentence=\"chose by harness, recommended\""),
            "{chrome}"
        );
        let v = app.presence_view(wid).expect("a window");
        assert_eq!(
            v.ripple_chose,
            v.ripple_at.is_some(),
            "a pulse started by a choice is marked as one (and none starts under reduced motion)"
        );
        // An approval told the same way is still the WATCHER's.
        assert_eq!(app.tell_story(sid, StoryVerb::Approval, ""), Ok(2));
        assert!(
            app.presence_chrome_line()
                .ends_with(" sentence=\"approved by watcher\""),
            "{}",
            app.presence_chrome_line()
        );
    }

    /// The choice pulse paints on a window with no rim, in the story tone,
    /// decays over the ripple's life and leaves nothing behind (overlay `None`,
    /// stamp and mark cleared by the tick). With the rim off it paints nothing.
    #[test]
    fn the_choice_pulse_paints_on_a_rimless_window_and_then_goes_quiet() {
        let (mut app, wid, _sid, _ctx) = app_with_stub();
        let now = Instant::now();
        assert!(app.presence_overlay(wid, now).is_none(), "quiet before");
        {
            let ws = app.windows.get_mut(&wid).expect("window");
            ws.presence.ripple_at = Some(now);
            ws.presence.ripple_chose = true;
        }
        let tones = crate::chrome_band::presence_tones(app.chrome_palette_theme());
        let first = app.presence_overlay(wid, now).expect("the pulse paints");
        assert_eq!(first.accent, pack(tones.story), "the story tone");
        assert_eq!(first.border_a, 255, "the edge flashes to full");
        let late = app
            .presence_overlay(wid, now + presence::RIPPLE - Duration::from_millis(1))
            .expect("still running");
        assert!(late.wash_a < first.wash_a, "the wash decays");
        assert_ne!(app.presence_fp(wid, now), 0, "a running pulse repaints");
        // The rim switched off: the pulse is a rim flash, so none is painted.
        app.presence_rim_on = false;
        assert!(
            app.presence_overlay(wid, now).is_none(),
            "rim off, no pulse"
        );
        app.presence_rim_on = true;
        let after = now + presence::RIPPLE;
        assert!(app.presence_overlay(wid, after).is_none(), "quiet after");
        app.presence_tick(after);
        let v = app.presence_view(wid).expect("window");
        assert!(
            v.ripple_at.is_none() && !v.ripple_chose,
            "the tick retires it"
        );
        // A turn-submit ripple on a rim-less window still paints nothing: only
        // a choice pulse is allowed past the quiet test.
        app.windows
            .get_mut(&wid)
            .expect("window")
            .presence
            .ripple_at = Some(after);
        assert!(app.presence_overlay(wid, after).is_none());
    }

    /// The choice pulse shows on every window whose front tab is the answered
    /// session, including windows without OS key focus. Controls: the
    /// turn-submit ripple on the unfocused window still does not start, and
    /// reduced motion still stops the pulse.
    #[test]
    fn a_choice_pulses_a_window_that_is_not_the_key_window() {
        crate::fabric::with_link_reset(a_choice_pulses_a_window_that_is_not_the_key_window_body);
    }

    fn a_choice_pulses_a_window_that_is_not_the_key_window_body() {
        let (mut app, wid, sid, _ctx) = app_with_stub();
        app.system_reduce_motion = false;
        app.windows.get_mut(&wid).expect("window").focused = false;
        let now = Instant::now();
        drive::ripple(&mut app, wid, now);
        assert!(
            app.presence_view(wid)
                .expect("a window")
                .ripple_at
                .is_none(),
            "a turn-submit ripple stays still on an unfocused window"
        );
        assert!(app.tell_story(sid, StoryVerb::Chose, "recommended").is_ok());
        let v = app.presence_view(wid).expect("a window");
        assert!(
            v.ripple_at.is_some() && v.ripple_chose,
            "the choice pulse starts on a window that is not the key window"
        );
        // Reduced motion: none.
        let (mut app, wid, sid, _ctx) = app_with_stub();
        app.system_reduce_motion = true;
        app.windows.get_mut(&wid).expect("window").focused = false;
        assert!(app.tell_story(sid, StoryVerb::Chose, "recommended").is_ok());
        assert!(
            app.presence_view(wid)
                .expect("a window")
                .ripple_at
                .is_none(),
            "reduced motion stops the pulse"
        );
    }

    /// A window holding the answered session only in a background tab gets no
    /// pulse (it would name the wrong tab), though the chime still plays.
    #[test]
    fn a_background_tab_choice_pulses_no_rim() {
        crate::fabric::with_link_reset(a_background_tab_choice_pulses_no_rim_body);
    }

    fn a_background_tab_choice_pulses_no_rim_body() {
        let (mut app, wid, sid, _ctx) = app_with_stub();
        app.trail_audio = crate::trail_audio::TrailAudio::capturing_for_test();
        let other = app.next_session_id;
        app.push_stub_tab(wid, crate::stub_session(other));
        assert_ne!(app.focused_session_id(wid), Some(sid), "sid is now behind");
        assert!(app.tell_story(sid, StoryVerb::Chose, "recommended").is_ok());
        let v = app.presence_view(wid).expect("a window");
        assert!(
            v.ripple_at.is_none() && !v.ripple_chose,
            "no rim for a background tab"
        );
        assert_eq!(
            app.trail_audio.take_captured_for_test().len(),
            1,
            "the chime plays for a background tab"
        );
    }

    /// A carried lease lapses on time after Commit: the handoff seeds another
    /// supervisor's `ttl=` lease on an adopted session before it is registered,
    /// so `arm_supervisor_expiries` arms the presence timer at its expiry. A
    /// dead holder's lease is lapsed and said (`meta-change field=supervisor
    /// value=-`), telling this instance's parked supervisor host to take the
    /// session.
    #[cfg(unix)]
    #[test]
    fn a_carried_lease_is_lapsed_on_time_after_the_commit() {
        let (mut app, _wid, _sid, ctx) = app_with_stub();
        crate::session_timeline::seed_carried_claims(
            &ctx.meta,
            &ctx.timeline,
            Some("watch-bob 30"),
            &[],
            crate::metrics::now_us(),
        );
        assert!(ctx.meta.lock().unwrap().supervisor.is_some(), "seeded");
        std::thread::sleep(Duration::from_millis(60));
        let _ = app.presence_tick(Instant::now());
        assert!(
            ctx.meta.lock().unwrap().supervisor.is_some(),
            "a seed arms nothing by itself: the tick past the expiry misses it"
        );
        app.arm_supervisor_expiries();
        assert!(
            app.presence_deadline(Instant::now()).is_some(),
            "armed at the lapse"
        );
        let _ = app.presence_tick(Instant::now() + Duration::from_millis(5));
        assert!(
            ctx.meta.lock().unwrap().supervisor.is_none(),
            "the lease is lapsed at its expiry"
        );
        let lapsed = ctx
            .timeline
            .lock()
            .unwrap()
            .since(None)
            .filter(|e| e.kind == "meta-change" && e.payload == "field=supervisor value=-")
            .count();
        assert_eq!(lapsed, 1, "and said");
    }

    /// The row's commit holds while the sensed mark differs from the live one
    /// (`drive::commit_rows`), so both must be one reduction of the lease. Every
    /// lease state (none, typing turn, settling turn, live drive lease, lapsed
    /// lease, driving a peer's turn) reads the same through `presence_facts`
    /// and `Desk::lease_mark`.
    #[test]
    fn the_sensed_lease_mark_is_the_live_one_in_every_lease_state() {
        let (mut app, wid, sid_a, ctx_a) = app_with_stub();
        let sid_b = app.next_session_id;
        app.push_stub_tab(wid, crate::stub_session(sid_b));
        let ctx_b = app.pool.get(sid_b).unwrap().ctx.clone();
        let now = crate::metrics::now_us();
        let marks = |app: &App, sid: u64| {
            (
                app.presence_facts(sid).expect("pooled").lease,
                Desk::lease_mark(app, sid).expect("pooled"),
            )
        };
        let states = [
            (None, LeaseMark::Free),
            (
                Some(crate::Lease::Turn {
                    id: 7,
                    driver: None,
                    typing: true,
                }),
                LeaseMark::Typing,
            ),
            (
                Some(crate::Lease::Turn {
                    id: 7,
                    driver: None,
                    typing: false,
                }),
                LeaseMark::Settling,
            ),
            (
                Some(crate::Lease::Drive {
                    holder: "rig".into(),
                    expires_us: now + 60_000_000,
                    conn: None,
                    hard: false,
                }),
                LeaseMark::Typing,
            ),
            (
                Some(crate::Lease::Drive {
                    holder: "rig".into(),
                    expires_us: now.saturating_sub(1),
                    conn: None,
                    hard: false,
                }),
                LeaseMark::Free,
            ),
        ];
        for (lease, want) in states {
            *ctx_a.turn_lease.lock().unwrap() = lease.clone();
            assert_eq!(marks(&app, sid_a), (want, want), "{lease:?}");
        }
        // A drives B's turn: A's band reads `▸ @B`, and so do both marks.
        *ctx_a.turn_lease.lock().unwrap() = None;
        *ctx_b.turn_lease.lock().unwrap() = Some(crate::Lease::Turn {
            id: 8,
            driver: Some(ctx_a.self_id.clone()),
            typing: true,
        });
        assert!(matches!(
            app.presence_facts(sid_a).unwrap().hand,
            Hand::Driving { .. }
        ));
        assert_eq!(marks(&app, sid_a), (LeaseMark::Driving, LeaseMark::Driving));
        assert_eq!(marks(&app, sid_b), (LeaseMark::Typing, LeaseMark::Typing));
    }

    /// A panic inside a chrome re-grid batch ends the batch: the re-grid it
    /// had recorded is applied, and the next one is applied at once rather
    /// than recorded into a batch nobody ends.
    #[test]
    fn a_panic_inside_a_chrome_regrid_batch_ends_the_batch() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let _ = take_chrome_regrids_applied();
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            app.with_chrome_regrid_batch(|app| {
                app.regrid_window_for_chrome_rows(wid);
                assert!(take_chrome_regrids_applied().is_empty(), "recorded");
                panic!("inside the batch");
            })
        }));
        assert!(caught.is_err(), "the panic resumed");
        assert!(app.chrome_regrid_batch.is_none(), "the batch ended");
        assert_eq!(
            take_chrome_regrids_applied().len(),
            1,
            "what it recorded was applied"
        );
        app.regrid_window_for_chrome_rows(wid);
        assert_eq!(
            take_chrome_regrids_applied().len(),
            1,
            "the next re-grid applies at once"
        );
    }

    /// Inside a batch that recorded a window's re-grid, a chrome commit
    /// measures against the rows that re-grid gives, not the stale grid;
    /// outside one, or for a window it did not record, against the grid.
    #[test]
    fn a_chrome_commit_inside_a_batch_measures_the_pending_regrid() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let other = WindowId(1);
        let size = winit::dpi::PhysicalSize::new(1600, 1200);
        let gridded = app.grid_dims_for(wid, size).0;
        assert!(gridded > 2, "a window with rows to spare");
        // The band commits a row: `ws.rows` still reads the old grid.
        app.message_band_rows += 1;
        assert_eq!(
            app.chrome_commit_grid_rows_at(wid, gridded, Some(size)),
            gridded,
            "outside a batch the grid re-grids at once, so it is current"
        );
        app.chrome_regrid_batch = Some(ChromeRegridBatch::default());
        app.regrid_window_for_chrome_rows(wid);
        assert_eq!(
            app.chrome_commit_grid_rows_at(wid, gridded, Some(size)),
            gridded - 1,
            "the pending re-grid gives a row fewer"
        );
        assert_eq!(
            app.chrome_commit_grid_rows_at(other, gridded, Some(size)),
            gridded,
            "a window the batch did not record keeps its grid"
        );
        assert_eq!(
            app.chrome_commit_grid_rows_at(wid, gridded, None),
            gridded,
            "a headless window never re-grids for chrome"
        );
        app.chrome_regrid_batch = None;
        let _ = take_chrome_regrids_applied();
    }

    // ---- round 44: the second adversarial hunt's host repros -------------

    /// Under High Contrast the band is keyed on the resolved inks and the
    /// latch, so after a palette toggle the presence row's cached row is the
    /// one painted under the forced palette.
    #[test]
    fn r44_hb2_the_presence_row_repaints_under_a_high_contrast_toggle() {
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
        let plain = app.presence_band_row(wid, 120, theme).to_vec();
        assert_eq!(plain.len(), 120, "a presence row is up");
        let words = app.windows[&wid]
            .presence
            .words
            .clone()
            .expect("the row's words");
        let (name, palette) = crate::chrome_band::hc_fixtures::STOCK[0];
        crate::chrome_band::hc_fixtures::with_forced(palette, || {
            let forced = crate::message_band::paint_presence_row(&words, 120, theme);
            assert_ne!(forced, plain, "{name}: the palette moves the row's cells");
            let shown = app.presence_band_row(wid, 120, theme).to_vec();
            assert!(
                shown == forced,
                "{name}: the presence row kept the theme's palette under High Contrast"
            );
        });
    }
}
