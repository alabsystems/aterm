// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The App's side of PRESENCE ([`crate::presence`]): SENSING each session's
//! facts from its own leaf locks, and being the engine driver's host. The
//! sequencing — fold the facts into the per-session slot, project the FOCUSED
//! session of every window onto its view, commit the row to the geometry, the
//! fold law, the ripple and the pulse, a told story, the deadline and the
//! tick — is `aterm_messages::presence::drive` (design ruling 353); this file
//! implements its [`Desk`] on the App: the window walk, the facts, the
//! on-screen test, the motion policy, the re-grid, the repaint, the chips,
//! the herald and the chime. Round 30 moved the pure core (ruling 348).
//!
//! Every entry point here runs at CHANGE rate: a `Wake::LeaseChanged` /
//! `Wake::FabricChanged` / `Wake::MetaChanged`, a status-revision or
//! agent-verdict move from the sweep, a tab switch, the human's own keystroke (the fold law), and the
//! band's once-a-second / once-a-minute `since` tick while a row is up. The
//! frame path reads plain fields on the view and allocates nothing
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

/// The chip level the existing indicator bits spell — the engine's
/// (`aterm_messages::presence::chip_of_attention`), under the name the tab
/// strip has always used.
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

/// How many window re-grids the presence row has paid for ON THIS THREAD
/// (test-only: the fold-law test's "one PTY resize per transition"; a
/// headless App is driven on its test's own thread).
#[cfg(test)]
pub(crate) fn presence_regrids() -> u64 {
    PTY_RESIZES_FOR_PRESENCE.with(|c| c.get())
}

/// A session's own lease reduced to what the row's geometry hangs on
/// ([`LeaseMark`]): the one reduction both the sensed facts and the live
/// [`Desk::lease_mark`] use, so the two compare like for like. A drive lease
/// past its expiry is `Free`, as `presence_facts` reads it.
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

    /// Gather one session's facts. Leaf locks only, each held for a copy; the
    /// agent reading is the status sweep's published one (no terminal lock
    /// here at all).
    fn presence_facts(&self, session: u64) -> Option<Facts> {
        let s = self.pool.get(session)?;
        let ctx = &s.ctx;
        let now_us = crate::metrics::now_us();
        let now = Instant::now();
        let (hand, lease_until, typing, mark) = {
            let lease = ctx.turn_lease.lock().unwrap_or_else(|p| p.into_inner());
            let typing = lease.as_ref().is_some_and(|l| l.driver_may_type(now_us));
            // The mark from THIS read of the lease, never a second lock of
            // it: the words and the mark they are committed under must be
            // one moment of the lease (`drive::commit_rows`).
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
            // ONE PLACE TELLS IT (ruling 270; day three): a stalled agent
            // upgrade is the message band's row and the log's record, with
            // its words and its buttons. The harness also raises the tab's
            // attention, and that stays a FACT — the level, `status
            // why=escalation`, the tab chip and the rim all keep it, so the
            // stall still shows at the top once its row folds. Only the
            // presence line's phase slot does not repeat words the band has
            // already said; any other owner's attention still reads there.
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
        // THE SERVER'S PUBLISHED VERDICT: the status sweep classified the
        // screen (only for an identified agent, only when its live zone
        // changed); presence folds that reading in and never re-reads the
        // screen itself, so the band, the rim, `status agent=` and `EVENT
        // agent` all say the same thing.
        let (agent_seq, agent) = self.session_status.agent_reading(session);
        // THE LOOP'S NEXT TRY, laid on an API wall (`with_retry_plan`): the
        // tab says `→ 14:05 · 3m` beside `can't reach the API`. The wall, its
        // band word and `status agent=` stay the screen's.
        let agent = match self.harness_waits.get(ctx.self_id.as_str()) {
            None => agent,
            Some(&at) => presence::with_retry_plan(
                agent,
                Some(at),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0)),
                now,
                presence::local_offset_s,
            ),
        };
        // The published input stall (`input_stall::InputWatches`): the fact
        // the frozen program's unmoving screen cannot carry.
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
    /// onto every window that shows the session (`drive::refresh_session`).
    /// `submitted` marks a turn's submit keypress — the ripple's edge.
    pub(crate) fn refresh_presence_session(&mut self, session: u64, submitted: bool) {
        drive::refresh_session(self, session, submitted);
    }

    /// The harness loop's NEXT TRY for `sid` ([`crate::Wake::HarnessWait`]):
    /// kept, and the tab's words re-read where it changed — a plan told
    /// after the wall was already on the tab must reach it, and a cleared one
    /// must not linger.
    pub(crate) fn apply_harness_wait(&mut self, sid: &str, at_unix: Option<i64>) {
        let changed = match at_unix {
            Some(at) => self.harness_waits.insert(sid.to_string(), at) != Some(at),
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

    /// THE FOLD LAW (`drive::human_acted`): the human acted (a key, a click)
    /// in `wid`; a CALM session's story is read and its row folds.
    pub(crate) fn note_human_acted(&mut self, wid: WindowId) {
        drive::human_acted(self, wid);
    }

    /// THE FOLD LAW for a press (`drive::human_acted_on`, ruling 372): the
    /// human acted in `wid` while `session` was its front — the story they
    /// SAW, read even when the press brought another pane or a tab with no
    /// session front.
    pub(crate) fn note_human_acted_on(&mut self, wid: WindowId, session: u64) {
        drive::human_acted_on(self, wid, session);
    }

    /// Arm the presence timer at every pooled session's `ttl=` supervisor
    /// claim (round four, item 9) — called at a successor's Commit. A lease a
    /// seamless update carried was seeded before its session was registered,
    /// so no `meta set` wake ever armed its lapse; without this a lease whose
    /// holder died with the old instance was never lapsed, never said, and
    /// this instance's own supervisor host — parked behind it until a claim
    /// is released or lapses — never took the session. A refresh reads each
    /// claim (`drive::refresh_session`).
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
        // The engine's arithmetic (`View::rim_glow`). The quiet frame answers
        // before the tones are derived: the four contrast-floored tones cost
        // ~1.9 µs (measured, `adv8`), and every composed frame of every
        // window took it for a `None` (round 19's review, C1). A rim pays it;
        // a quiet window pays the match. A running CHOICE PULSE is the one
        // ripple a rim-less window paints.
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

    /// Re-grid ONE window after its chrome rows moved (the per-window twin of
    /// `regrid_for_chrome_rows`): the strip cache and the present key are
    /// cleared, and the window is re-gridded from its own OS size — which is
    /// the one PTY resize.
    ///
    /// A resize ENTRY POINT for the ledger (`site=chrome`, the outermost one,
    /// so the `on_resize` below books under it): this shim alone is
    /// `#[track_caller]`.
    #[track_caller]
    pub(crate) fn regrid_window_for_chrome_rows(&mut self, wid: WindowId) {
        let _site = crate::resize_ledger::SiteScope::enter(crate::resize_ledger::Cause::Chrome);
        self.regrid_window_for_chrome_rows_inner(wid);
    }

    /// The body of [`Self::regrid_window_for_chrome_rows`].
    fn regrid_window_for_chrome_rows_inner(&mut self, wid: WindowId) {
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

    /// `aterm ctl story <verb> [<text>]` landed for `session`
    /// (`drive::tell`): the slot is brought current, the point is noted, and
    /// every window showing the session is re-projected — the phase slot reads
    /// the verb for [`presence::TOLD_FLASH`], then returns to the phase on the
    /// timer; a `chose` pulses and chimes. The reply is the point's seq; `Err`
    /// for a session the pool no longer holds.
    pub(crate) fn tell_story(
        &mut self,
        session: u64,
        verb: StoryVerb,
        text: &str,
    ) -> Result<u64, &'static str> {
        drive::tell(self, session, verb, text).ok_or("no such session")
    }

    /// THE CHOICE CHIME: one short, quiet pip (the output voice's `Shimmer`,
    /// which every palette voices at a whisper) when the supervisor answers a
    /// question box by policy. Gate order is load-bearing, as the bell's is
    /// (`on_bell`): serious mode, then the Music effects master
    /// (`trail_sounds` — the chime is a SYNTH voice, and the Sound box promises
    /// that muting that master silences every synth voice; only the OS bell is
    /// outside it), then `choice_sound`, then the rate limiter LAST —
    /// `try_fire` consumes its token, so a muted chime must not spend it. No
    /// focus test, unlike the trail's own cues: the point is to hear that a
    /// background session's question was answered. The audio host is already
    /// inert headless and in tests (`TrailAudio::new(false)`), and a zero
    /// volume pushes nothing and spends no token.
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

    /// The claim's lapse on the microsecond clock, placed on `now`'s: one
    /// millisecond past it, so the lapse check at the wake sees it lapsed on
    /// the microsecond clock too.
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

    /// Read from the LIVE lease of every terminal session `w` hosts, in any
    /// tab — never a slot a wake has not refreshed yet: the window's re-grid
    /// resizes all of them.
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
    /// of every terminal session `w` hosts, in any tab — the window's re-grid
    /// resizes all of them — as an instant on the Desk's clock.
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
        // The stamp's own instant, placed off the metrics clock's one anchor:
        // no second clock read, so `deadline` and the `tick` it arms see the
        // SAME look (a mapping through `now_us() - last` and `clock()`, two
        // reads microseconds apart, moved the look's due time between them).
        crate::metrics::instant_at_us(last?)
    }

    fn grid_rows(&self, w: WindowId) -> Option<(u16, bool)> {
        self.windows
            .get(&w)
            .map(|ws| (ws.rows, ws.os_window.is_some()))
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

    /// The lapsed claim is removed and recorded (`meta-change
    /// field=supervisor value=-`, pushed as `EVENT meta`); the driver's
    /// re-read then re-heralds the session, so a box the dead supervisor was
    /// holding reaches the menu row and the notification.
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

/// Whether the choice chime may speak at all, before its rate limiter: serious
/// mode allows terminal sound, the Music effects master is on, AND
/// `choice_sound` is on. Pure, so the gate is testable where the audio host is
/// inert.
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
    //! Item 7 of the round-19 contract, the slice this commit lands: the
    //! headless model, the fold law, the idle repaint invariant, the classifier
    //! gate, the golden cells, the chip level and the a11y sentence. The rim
    //! images ride the sacred `image` path in `app_introspect`'s capture tests.

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

    /// THE HEADLESS MODEL TEST (§7): a turn submit puts the rim on and the hand
    /// in the band before any status revision moves; a hold is the stop rim and
    /// `⊘ hold` within one wake; a delivery prints `✉1 task←✓x` with the trust
    /// glyph BEFORE the sender; a hold lands only on the session it names — a
    /// second session in the same window stays quiet (`bridge_lost` holds only
    /// the sessions the bridge touched, and this is the read side of that).
    #[test]
    fn a_turn_a_hold_and_a_delivery_reach_the_rim_and_the_band_at_change_rate() {
        // Reads the process-global link through `presence_level`; takes the
        // reset like every other reader (see review_r1's note, 2026-09-20).
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

    /// THE FOLD LAW (§7): a keypress while the session is not calm is not a
    /// fold; calm without a keypress (any number of ticks) is not a fold; both
    /// together fold the row — and the fold is ONE PTY re-grid.
    #[test]
    fn the_row_folds_only_on_a_keypress_while_calm_and_pays_one_regrid() {
        // Reads the process-global link through `presence_level`; takes the
        // reset like every other reader (see review_r1's note, 2026-09-20).
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

        // The key, while calm: the fold, and exactly one re-grid. The row has
        // stood the two minutes the ticks above played — past its minimum
        // life (ruling 394: a key folds a row only once it has stood
        // `FOLD_QUIET`); the App's clock is the wall's, so the birth is
        // dated back to match.
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

    /// A CLICK LANDS WHERE THE PERSON SAW IT (ruling 369; day nine's fix
    /// stage, live): with a story row up above the message band, a click on
    /// a band capsule presses that capsule, THEN reads the story. The fold
    /// law ran first and its re-grid moved the band up a row under the
    /// pointer, so the press landed on the grid and the capsule never acted.
    /// Control: the story still folds on that click.
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
            // The App's clock is the wall's: the row is dated born NOW, just
            // before the press, so the setup above (a debug build, a loaded
            // machine) never ages it past `FOLD_QUIET` and turns the young
            // row this test reads into one the click may fold at once.
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
            // The capsule opened Settings: no session is front now, so the
            // front-reading law would have read nothing here (the old
            // "Quiet" check passed on the Settings tab alone). The story
            // the person was looking at is read (ruling 372).
            assert_eq!(app.focused_session_id(wid), None, "Settings is front");
            let seq = app.presence.slot(sid).unwrap().story_seq;
            assert!(seq > 0);
            assert_eq!(
                app.presence_view(wid).unwrap().watermark(sid),
                seq,
                "the story the person saw is read"
            );
            // The read folds the row once it has stood its minimum life
            // (ruling 394, composed with ruling 369 at the integration): a
            // row born just now stands until born + `FOLD_QUIET`, where the
            // presence timer folds it with one re-grid — AFTER the press, so
            // the band never moved under the pointer either way.
            let young = app.presence_view(wid).unwrap();
            assert_eq!(young.rows, 1, "a young row waits out its minimum life");
            let due = young.born_at.expect("born") + FOLD_QUIET;
            assert_eq!(app.presence_deadline(Instant::now()), Some(due));
            let _ = app.presence_tick(due);
            assert_eq!(app.presence_view(wid).unwrap().rows, 0);
        });
    }

    /// A PRESS THAT MOVES THE FRONT READS THE STORY THE PERSON SAW (ruling
    /// 372), through the one sequence the event loop's `MouseInput` arm
    /// runs (`mouse_input_then_fold`): a click into the other split pane
    /// reads the story of the pane that was front and leaves the pane it
    /// focused — whose story nobody has seen — unread, its row up.
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
            // The next act in the pane now in front reads it.
            // The row has stood its minimum life (ruling 394: a read folds a
            // row only once it has stood `FOLD_QUIET`); the App's clock is
            // the wall's, so the birth is dated back to match.
            let born = &mut app.windows.get_mut(&wid).unwrap().presence.born_at;
            *born = born.and_then(|b| b.checked_sub(FOLD_QUIET));
            app.mouse_input_then_fold(wid, ElementState::Pressed, MouseButton::Left);
            assert_eq!(app.presence_view(wid).unwrap().watermark(top), seq_top);
            assert_eq!(app.presence_view(wid).unwrap().rows, 0);
            app.mouse_input_then_fold(wid, ElementState::Released, MouseButton::Left);
        });
    }

    /// THE FOLD QUIET (ruling 394, proposed), on the real App: a settling
    /// turn's lease born and dropped inside a millisecond — the incident's
    /// schedule (measured 2026-09-28: 121x52 -> 51 -> 52 within ~0.8 ms, an
    /// alt-screen Claude Code left stale) — pays ONE PTY re-grid, the birth,
    /// not two; a second lease inside the quiet pays none; the row stands
    /// blank with an empty `chrome` band meanwhile; and the timer the
    /// presence deadline arms folds it once, one re-grid, after the quiet.
    #[test]
    fn a_quick_show_and_fold_pays_one_regrid_not_two() {
        // Reads the process-global link through `presence_level`; takes the
        // reset like every other reader (see review_r1's note, 2026-09-20).
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

    /// THE TAB RETRY PLAN, THROUGH THE WINDOW (2026-09-29): the status sweep
    /// publishes an API wall for the front session (the measured tall-pane
    /// screen, `agent=wall:api-error`); the loop's next try, told over
    /// `Wake::HarnessWait`, lands on THAT tab's words — `→ <clock> · <left>`
    /// on the band, `next try` in the sentence — and clearing it, or a plan
    /// already in the past, takes it away again. CONTROLS: the wall's band
    /// word is the screen's either way, and a plan told for another session
    /// changes this tab's row not at all.
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

            let unix = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0));
            app.apply_harness_wait(&mine, Some(unix + 180));
            let told = app.presence_chrome_line();
            assert!(told.contains("can't reach the API"), "{told}");
            assert!(told.contains('\u{2192}'), "the band has the try: {told}");
            assert!(told.contains("next try"), "the sentence says it: {told}");

            // Cleared, or already past: the row is as the screen alone made it.
            app.apply_harness_wait(&mine, None);
            assert_eq!(app.presence_chrome_line(), bare);
            app.apply_harness_wait(&mine, Some(unix - 60));
            assert_eq!(app.presence_chrome_line(), bare, "a past try shows nothing");
            // A plan for another session is not this tab's.
            app.apply_harness_wait("s-not-this-one", Some(unix + 180));
            assert_eq!(app.presence_chrome_line(), bare);
        });
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
        // Reads the process-global link (a sibling test's un-acked bridge
        // reads as `level=note` and a `~` band), so it takes the reset like
        // every other reader.
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

    /// THE CLASSIFIER GATE (§7), presence's half: a refresh folds the
    /// sweep's PUBLISHED reading and never classifies — a thousand refreshes
    /// cost zero `aterm_phase` calls, and a status revision move costs none
    /// either (the audit measured `level=quiet` with a box up because the old
    /// gate hung the classifier on that revision). The sweep's half — one
    /// classification per changed live zone, at most 4 Hz — is pinned in
    /// `session_status`.
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

    /// THE GOLDEN CELLS (§7): every band row at 120, 60 and 24 columns, painted
    /// in light, dark and each stock Windows High-Contrast palette. The 120-col
    /// line is literal; at every width the painted text IS the fitted words
    /// (one margin cell each side), every cell sits on the band, every ink
    /// clears the band, the emoji-capable glyphs are pinned to text
    /// presentation, and hand + phase + mail (where there is any, ruling 366)
    /// survive to 24 columns.
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

    /// The chip is a LEVEL: the indicator bit spells `Wait`, and the presence
    /// stamp folds the session's level over it — a hold is the filled diamond,
    /// a story the dot, and a quiet session leaves the bit alone.
    #[test]
    fn the_chip_level_folds_the_session_level_over_the_indicator_bit() {
        // Reads the process-global link (a sibling test's un-acked bridge
        // reads as `level=note` and a `~` band), so it takes the reset like
        // every other reader.
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
        // The link is process-global (`fabric_state()`), and a `Quiet` verdict
        // is exactly the one a sibling's attached or lost bridge turns into
        // `Note`: measured on the merge gate's parallel run of this binary,
        // 2026-09-20, this test read `Note` where it asserts `Quiet` and
        // passed alone. Every test that reads the link takes the reset
        // (`with_link_reset`'s own rule), the readers included.
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

    /// R1b: the same root — a story the human READ does not come back after
    /// they read a second tab.
    #[test]
    fn review_r1b_a_story_already_read_does_not_return_on_a_tab_switch() {
        // Reads the process-global link through `presence_level`; takes the
        // reset like every other reader (see review_r1's note, 2026-09-20).
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

    /// R2: a cooperative lease that LAPSES (its driver crashed) posts no wake;
    /// the model arms its expiry as a deadline and the tick re-reads the hand,
    /// so the teal rim and `◂ holder` lift when `lease status` says `none`.
    #[test]
    fn review_r2_a_lapsed_drive_lease_lifts_the_rim() {
        // Reads the process-global link (a sibling test's un-acked bridge
        // reads as `level=note` and a `~` band), so it takes the reset like
        // every other reader.
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
        // Reads the process-global link (a sibling test's un-acked bridge
        // reads as `level=note` and a `~` band), so it takes the reset like
        // every other reader.
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

    /// R5: a `since` that prints seconds — `3m12s` included — ticks every
    /// second, never once a minute with the figure stale by up to 59 s. (Which
    /// clauses print seconds is the engine's law, proved in its own tests.)
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

    /// THE WORDS KEEP THEIR OWN CLOCK (audit 2026-09-24): another owner's
    /// wake before the words are due recomposes nothing, and a band nobody
    /// can see — occluded, minimized, headless — arms no text tick at all.
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

    /// Review 2026-09-24: words that change OUTSIDE the tick (a wake, a tab
    /// switch) re-arm their clock — a minutes row's +60 s due must not freeze
    /// the seconds row that replaced it.
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
        // Reads the process-global link (a sibling test's un-acked bridge
        // reads as `level=note` and a `~` band), so it takes the reset like
        // every other reader.
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

    /// R8: a turn is attributed only to the session the dispatch resolved as
    /// its driver (`Lease::Turn::driver`, the source of the edge the turn
    /// came over) — a read-screen observer watching the session cannot have
    /// typed it, and neither can a write edge that merely stands (see
    /// `adv3_…` for the seam itself).
    #[test]
    fn review_r8_a_turn_is_not_attributed_to_a_read_only_observer() {
        // Reads the process-global link (a sibling test's un-acked bridge
        // reads as `level=note` and a `~` band), so it takes the reset like
        // every other reader.
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
        // Reads the process-global link (a sibling test's un-acked bridge
        // reads as `level=note` and a `~` band), so it takes the reset like
        // every other reader.
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
        // Reads the process-global link (a sibling test's un-acked bridge
        // reads as `level=note` and a `~` band), so it takes the reset like
        // every other reader.
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
            let wait_for_lease = |ctx: &crate::SessionCtx| -> u64 {
                for _ in 0..200 {
                    if let Some(crate::Lease::Turn { id, .. }) =
                        ctx.turn_lease.lock().unwrap().as_ref()
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
        });
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

    /// `ctl story chose <policy>` plays ONE chime — the output voice's
    /// `Shimmer`, centred, at the synth volume, never feeding the bed — and a
    /// second choice inside the two-second floor plays none. A story that is
    /// not a choice plays nothing.
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

    /// A muted chime never spends the limiter's token (the bell's gate-order
    /// law): with `choice_sound = false`, with the Music effects master off, at
    /// zero volume, or in serious mode, a `chose` story plays nothing AND leaves the
    /// two-second token in place for the next chime that may speak.
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

    /// `ctl story chose <policy>`: the band tells it with its own glyph and the
    /// spoken sentence names the HARNESS, not a watcher; the quiet summary
    /// counts it as a choice, apart from approvals; any pulse it starts is a
    /// choice pulse.
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

    /// THE CHOICE PULSE paints on a window with NO rim — the one ripple a quiet
    /// window shows — in the story tone, decays over the ripple's life, and
    /// leaves nothing behind: the overlay is `None` after it and the tick
    /// clears both the stamp and the mark. With the presence rim switched off
    /// it paints nothing.
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

    /// REVIEW FINDING [8]: the choice pulse is promised on EVERY window whose
    /// front tab is the answered session — the window the person is not in
    /// included. It was gated on the window's OS key focus like decorative
    /// motion, so a background window got the chime and no pulse. NEGATIVE
    /// CONTROLS: the turn-submit ripple on the same unfocused window still
    /// does not start, and reduced motion still stops the pulse.
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

    /// A window that holds the answered session only in a BACKGROUND tab gets
    /// no rim pulse — the flash would name the wrong tab — though the chime
    /// still plays (bell-class: the point is to hear a background answer).
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

    /// A CARRIED LEASE IS LAPSED ON TIME AFTER THE COMMIT (the round-four
    /// plan, item 9). The handoff seeds another supervisor's `ttl=` lease on
    /// an adopted session before it is registered, so no `meta set` wake ever
    /// armed the presence timer at its expiry. The successor's Commit arms
    /// every session's (`arm_supervisor_expiries`): a holder that died with
    /// the old instance has its lease lapsed and said (`meta-change
    /// field=supervisor value=-`) when it runs out, which is what tells this
    /// instance's supervisor host — parked behind that claim — to take the
    /// session.
    ///
    /// FAILS WITHOUT THE FIX: nothing is armed; the tick past the expiry
    /// leaves the dead lease on the session for good (the host parked behind
    /// it is never told), and `arm_supervisor_expiries` does not exist.
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

    /// THE ROW'S COMMIT HOLDS WHILE THE SENSED MARK DIFFERS FROM THE LIVE ONE
    /// (`drive::commit_rows`), so the two must be ONE reduction of the lease:
    /// were they to disagree at rest, a window's row would never be committed.
    /// Every state of the lease — none, a turn typing, a turn settling, a live
    /// drive lease, a lapsed one, and a session driving a peer's turn — reads
    /// the same through `presence_facts` and `Desk::lease_mark`.
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
}
