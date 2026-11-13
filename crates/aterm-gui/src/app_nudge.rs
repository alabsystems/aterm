// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Host-side repaint nudge driver.

use std::collections::HashMap;

use aterm_core::terminal::repaint_nudge::{
    Act, Dropped, Facts, GiveUp, Mode, PTY_HOLD_CAP_MS, Phase, Reason, RepaintNudge,
    person_quiet_ms,
};

use crate::resize_epoch::NudgeView;

/// The adoption arm's epoch id.
pub(crate) const ADOPTION_EPOCH: u64 = u64::MAX;

const PTY_RETRY_MS: u64 = 250;

/// Grace period for an adopted agent tab to repaint before arming the nudge.
pub(crate) const ADOPTION_WAIT_MS: u64 = 1_500;

/// How long the class of a repaint-adopted tab is waited for.
pub(crate) const ADOPTION_CLASS_MS: u64 = 5_000;

static REPAINT_ADOPTIONS: std::sync::Mutex<Vec<u64>> = std::sync::Mutex::new(Vec::new());

/// `spawn_session` adopted `session` with a blank screen its program is expected to repaint.
#[cfg(unix)]
pub(crate) fn note_repaint_adoption(session: u64) {
    REPAINT_ADOPTIONS
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .push(session);
}

fn take_repaint_adoptions() -> Vec<u64> {
    std::mem::take(&mut *REPAINT_ADOPTIONS.lock().unwrap_or_else(|p| p.into_inner()))
}

fn repaint_adoptions_pending() -> bool {
    !REPAINT_ADOPTIONS
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .is_empty()
}

/// What a session's world says, minus the two facts only the host knows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct WorldFacts {
    /// Machine facts with [`Self::gesture_ms`] and default `mode`/`other_hold`.
    pub(crate) facts: Facts,
    /// Milliseconds since a person's last gesture on the session.
    pub(crate) gesture_ms: Option<u64>,
    /// The host's cell size in device pixels, for the winsize pixel fields.
    pub(crate) cell_px: Option<(u16, u16)>,
}

/// A PTY held one row short, released on drop.
pub(crate) struct PtyGuard {
    restore: Option<Box<dyn FnOnce()>>,
}

impl PtyGuard {
    /// A guard that runs `restore` when dropped unless [`Self::disarm`]ed.
    pub(crate) fn new(restore: impl FnOnce() + 'static) -> Self {
        Self {
            restore: Some(Box::new(restore)),
        }
    }

    /// The release was done by hand: the drop does nothing.
    pub(crate) fn disarm(mut self) {
        self.restore = None;
    }
}

impl Drop for PtyGuard {
    fn drop(&mut self) {
        if let Some(restore) = self.restore.take() {
            restore();
        }
    }
}

/// Everything the host reads from and does to the world.
pub(crate) trait NudgeWorld {
    fn now_ms(&self) -> u64;
    fn alive(&self, session: u64) -> bool;
    fn attached(&self, session: u64) -> bool;
    fn facts(&mut self, session: u64) -> Option<WorldFacts>;
    fn pty_dims(&self, session: u64) -> Option<(u16, u16)>;
    fn set_pty(&mut self, session: u64, rows: u16, cols: u16, cell_px: Option<(u16, u16)>);
    fn restore_guard(
        &self,
        session: u64,
        fallback: (u16, u16),
        cell_px: Option<(u16, u16)>,
    ) -> PtyGuard;
    fn log(&mut self, line: String);
    fn mark_tried(&mut self, session: u64, epoch: u64);
    /// The attempt for `epoch` held and the program answered no size: a repaint after it is the
    /// program's own (`by=app`), never the nudge's.
    fn unrepaired(&mut self, session: u64, epoch: u64);
    fn stamp(&mut self, session: u64, phase: &'static str, nudged: u32);
    fn publish(&mut self, session: u64);
    fn give_up(&mut self, session: u64, epoch: u64, why: GiveUp);
    fn superseded(&mut self, session: u64, epoch: u64);
    fn epoch_view(&self, session: u64) -> Option<NudgeView>;
}

struct NudgeDriver {
    machine: RepaintNudge,
    guard: Option<PtyGuard>,
    /// When this driver was made.
    attached_ms: u64,
    /// The next instant the machine asked to be stepped, if any.
    wake_ms: Option<u64>,
    /// Detail for the log lines.
    epoch_view: Option<NudgeView>,
    /// The cell box the hold was taken with, reused at the release.
    held_cell_px: Option<(u16, u16)>,
    /// What the ledger was last told.
    stamped: (Phase, u32),
    /// The last nudge's second answer, for `answered_before=`.
    last_answer_ms: Option<u64>,
}

struct Watch {
    /// When the reader was first seen attached and the session unfrozen.
    since_ms: Option<u64>,
    /// The engine's repaint counters then.
    base: (u64, u64),
}

/// The host: drivers per session, the held-PTY map, the mode.
pub(crate) struct NudgeHost {
    mode: Mode,
    /// Repaint adoptions under watch.
    watch: HashMap<u64, Watch>,
    drivers: HashMap<u64, NudgeDriver>,
    /// Sessions whose PTY is held one row short, and since when (timeline ms).
    pty_short: HashMap<u64, u64>,
}

impl Default for NudgeHost {
    fn default() -> Self {
        Self::new(Mode::default())
    }
}

impl NudgeHost {
    /// A host running `mode`.
    pub(crate) fn new(mode: Mode) -> Self {
        Self {
            mode,
            watch: HashMap::new(),
            drivers: HashMap::new(),
            pty_short: HashMap::new(),
        }
    }

    /// The mode chosen at launch.
    pub(crate) const fn mode(&self) -> Mode {
        self.mode
    }

    /// Nothing armed and nothing held: the loop has nothing to fold.
    pub(crate) fn is_idle(&self) -> bool {
        self.pty_short.is_empty()
            && self.watch.is_empty()
            && self.drivers.values().all(|d| {
                !matches!(
                    d.machine.phase(),
                    Phase::Armed | Phase::Short | Phase::Restoring
                )
            })
    }

    /// How many PTYs are held short right now (tests, `resizes`).
    #[cfg(test)]
    pub(crate) fn held(&self) -> usize {
        self.pty_short.len()
    }

    /// Arm the nudge for `session`'s `epoch` (or an adoption).
    pub(crate) fn arm<W: NudgeWorld>(
        &mut self,
        w: &mut W,
        session: u64,
        reason: Reason,
        epoch: u64,
    ) -> bool {
        if self.mode == Mode::Off {
            return false;
        }
        let now = w.now_ms();
        let view = w.epoch_view(session);
        let driver = self.drivers.entry(session).or_insert_with(|| NudgeDriver {
            machine: RepaintNudge::new(),
            guard: None,
            attached_ms: now,
            wake_ms: None,
            epoch_view: None,
            held_cell_px: None,
            stamped: (Phase::Idle, 0),
            last_answer_ms: None,
        });
        if !driver.machine.arm(self.mode, reason, epoch, now) {
            return false;
        }
        driver.epoch_view = view;
        driver.wake_ms = Some(now);
        true
    }

    /// Let every hold go before a handoff freeze.
    #[cfg(any(unix, test))]
    pub(crate) fn release_all<W: NudgeWorld>(&mut self, w: &mut W) {
        let now = w.now_ms();
        let sessions: Vec<u64> = self.drivers.keys().copied().collect();
        for session in sessions {
            self.let_go(w, session, now, "a handoff freeze");
        }
        let short: Vec<u64> = self.pty_short.keys().copied().collect();
        for session in short {
            self.reconcile_one(w, session);
        }
    }

    fn let_go<W: NudgeWorld>(&mut self, w: &mut W, session: u64, now: u64, why: &str) {
        let Some(driver) = self.drivers.get_mut(&session) else {
            return;
        };
        if !driver.machine.let_go(now) {
            return;
        }
        driver.guard = None;
        driver.wake_ms = Some(now);
        driver.stamped = (driver.machine.phase(), driver.machine.nudged());
        w.stamp(session, driver.stamped.0.wire(), driver.stamped.1);
        w.log(format!(
            "repaint nudge session={session} epoch={} released ({why}); waiting for the answer",
            driver.machine.epoch()
        ));
    }

    /// One session's held PTY: set it to the engine's size and forget it when the kernel agrees.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "RepaintNudge",
            action = "ReleaseByDeadline",
            project = "conformance_repaint_nudge::World::project"
        )
    )]
    fn reconcile_one<W: NudgeWorld>(&mut self, w: &mut W, session: u64) -> bool {
        if !w.alive(session) {
            self.pty_short.remove(&session);
            return true;
        }
        let cell_px = self.drivers.get(&session).and_then(|d| d.held_cell_px);
        let Some(facts) = w.facts(session) else {
            return false;
        };
        let engine = facts.facts.engine;
        let cell_px = facts.cell_px.or(cell_px);
        match w.pty_dims(session) {
            Some(pty) if pty != engine => {
                w.set_pty(session, engine.0, engine.1, cell_px);
                if w.pty_dims(session) == Some(engine) {
                    self.pty_short.remove(&session);
                    true
                } else {
                    false
                }
            }
            _ => {
                self.pty_short.remove(&session);
                true
            }
        }
    }

    /// Run every driver one step and settle the held-PTY map.
    pub(crate) fn tick<W: NudgeWorld>(&mut self, w: &mut W) -> Option<u64> {
        let now = w.now_ms();
        let mut next = self.tick_watch(w, now);
        let short: Vec<(u64, u64)> = self.pty_short.iter().map(|(s, t)| (*s, *t)).collect();
        for (session, since) in short {
            let driving = self
                .drivers
                .get(&session)
                .is_some_and(|d| d.machine.phase() == Phase::Short);
            let expired = now.saturating_sub(since) >= PTY_HOLD_CAP_MS;
            if driving && !expired {
                continue;
            }
            if driving {
                self.let_go(w, session, now, &format!("hold past {PTY_HOLD_CAP_MS} ms"));
            }
            self.reconcile_one(w, session);
        }
        let gone: Vec<u64> = self
            .drivers
            .keys()
            .copied()
            .filter(|s| !w.alive(*s))
            .collect();
        for session in gone {
            self.drivers.remove(&session);
            self.pty_short.remove(&session);
        }
        let sessions: Vec<u64> = self.drivers.keys().copied().collect();
        for session in sessions {
            let phase = self.drivers.get(&session).map(|d| d.machine.phase());
            if !matches!(phase, Some(Phase::Armed | Phase::Short | Phase::Restoring)) {
                if let Some(at) = self.drivers.get(&session).and_then(|d| d.wake_ms) {
                    next = Some(next.map_or(at, |n| n.min(at)));
                }
                continue;
            }
            if let Some(at) = self.step_one(w, session, now) {
                next = Some(next.map_or(at, |n| n.min(at)));
            }
        }
        for since in self.pty_short.values() {
            let at = since.saturating_add(PTY_HOLD_CAP_MS);
            let at = if at > now {
                at
            } else {
                now.saturating_add(PTY_RETRY_MS)
            };
            next = Some(next.map_or(at, |n| n.min(at)));
        }
        next
    }

    /// Watch a session `spawn_session` adopted blank ([`Self::tick_watch`]).
    pub(crate) fn watch_adoption(&mut self, session: u64) {
        if self.mode != Mode::Off {
            self.watch.entry(session).or_insert(Watch {
                since_ms: None,
                base: (0, 0),
            });
        }
    }

    fn tick_watch<W: NudgeWorld>(&mut self, w: &mut W, now: u64) -> Option<u64> {
        let mut next: Option<u64> = None;
        let sessions: Vec<u64> = self.watch.keys().copied().collect();
        for session in sessions {
            if !w.alive(session) {
                self.watch.remove(&session);
                continue;
            }
            let retry = now.saturating_add(50);
            let Some(world) = w.facts(session) else {
                next = Some(next.map_or(retry, |n| n.min(retry)));
                continue;
            };
            let Some(entry) = self.watch.get_mut(&session) else {
                continue;
            };
            let facts = world.facts;
            if !w.attached(session) || facts.frozen {
                entry.since_ms = None;
                continue;
            }
            let since = *entry.since_ms.get_or_insert(now);
            if entry.since_ms == Some(now) {
                entry.base = (facts.full_clears, facts.screen_replaced);
            }
            if facts.full_clears > entry.base.0 || facts.screen_replaced > entry.base.1 {
                self.watch.remove(&session);
                continue;
            }
            let waited = now.saturating_sub(since);
            if waited >= ADOPTION_WAIT_MS && facts.agent && facts.alt_screen {
                self.watch.remove(&session);
                let armed = self.arm(w, session, Reason::Adoption, ADOPTION_EPOCH);
                if armed {
                    w.log(format!(
                        "repaint nudge session={session} reason=adoption armed: no full repaint \
                         in {ADOPTION_WAIT_MS} ms of attach"
                    ));
                }
                continue;
            }
            if waited >= ADOPTION_CLASS_MS {
                self.watch.remove(&session);
                continue;
            }
            let at = since.saturating_add(if facts.agent {
                ADOPTION_WAIT_MS
            } else {
                ADOPTION_CLASS_MS
            });
            next = Some(next.map_or(at, |n| n.min(at)));
        }
        next
    }

    fn step_one<W: NudgeWorld>(&mut self, w: &mut W, session: u64, now: u64) -> Option<u64> {
        let other_hold = self.pty_short.keys().any(|held| *held != session);
        let Some(world) = w.facts(session) else {
            let driver = self.drivers.get_mut(&session)?;
            let at = now.saturating_add(50);
            driver.wake_ms = Some(at);
            return Some(at);
        };
        let driver = self.drivers.get_mut(&session)?;
        let mut facts = world.facts;
        facts.mode = self.mode;
        facts.other_hold = other_hold;
        facts.person_quiet_ms =
            person_quiet_ms(world.gesture_ms, now.saturating_sub(driver.attached_ms));
        let was = driver.machine.phase();
        let act = driver.machine.step(&facts, now);
        driver.wake_ms = None;
        let sid = session;
        let epoch = driver.machine.epoch();
        let reason = driver.machine.reason();
        match act {
            Act::Nothing => {}
            Act::WaitUntil(at) => driver.wake_ms = Some(at),
            Act::Hold => {
                let target = (facts.engine.0.saturating_sub(1), facts.engine.1);
                driver.held_cell_px = world.cell_px;
                let guard = w.restore_guard(sid, facts.engine, world.cell_px);
                w.set_pty(sid, target.0, target.1, world.cell_px);
                driver.guard = Some(guard);
                self.pty_short.insert(sid, now);
                w.mark_tried(sid, epoch);
                w.log(format!(
                    "repaint nudge session={sid} reason={} epoch={epoch} hold rows={} cols={}",
                    reason.word(),
                    target.0,
                    target.1
                ));
                driver.wake_ms =
                    Some(now.saturating_add(aterm_core::terminal::repaint_nudge::SETTLE_MS));
            }
            Act::Release => {
                let cell_px = driver.held_cell_px.or(world.cell_px);
                if let Some(guard) = driver.guard.take() {
                    guard.disarm();
                }
                w.set_pty(sid, facts.engine.0, facts.engine.1, cell_px);
                if w.pty_dims(sid).is_none_or(|pty| pty == facts.engine) {
                    self.pty_short.remove(&sid);
                } else {
                    w.log(format!(
                        "repaint nudge session={sid} release not confirmed; reconciling"
                    ));
                }
                let first = driver.machine.first_answer_ms();
                w.log(format!(
                    "repaint nudge session={sid} epoch={epoch} released first_answer_ms={}",
                    first.map_or_else(|| "-".to_string(), |ms| ms.to_string())
                ));
                driver.wake_ms = match driver.machine.phase() {
                    Phase::Restoring => {
                        Some(now.saturating_add(aterm_core::terminal::repaint_nudge::SETTLE_MS))
                    }
                    _ => None,
                };
            }
            Act::Forget => {
                // Someone else's layout pass set the PTY: it is theirs, so
                // nothing is written. The map entry goes once the kernel reads
                // the engine's size; until then the reconcile sets it.
                if let Some(guard) = driver.guard.take() {
                    guard.disarm();
                }
                if w.pty_dims(sid).is_none_or(|pty| pty == facts.engine) {
                    self.pty_short.remove(&sid);
                }
                w.log(format!(
                    "repaint nudge session={sid} epoch={epoch} hold ended by a resize to rows={} \
                     cols={}; waiting for the answer",
                    facts.pty.0, facts.pty.1
                ));
                driver.wake_ms =
                    Some(now.saturating_add(aterm_core::terminal::repaint_nudge::SETTLE_MS));
            }
            Act::WouldNudge => {
                let view = driver.epoch_view;
                w.log(format!(
                    "would-nudge session={sid} reason={} class=agent epoch={epoch} shift={:+} \
                     pushed={} cut={} drew={} answered_before={}",
                    reason.word(),
                    view.map_or(0, |v| v.shift),
                    view.map_or(0, |v| v.pushed),
                    view.map_or(0, |v| v.cut),
                    view.map_or(0, |v| u8::from(v.drew)),
                    driver
                        .last_answer_ms
                        .map_or_else(|| "-".to_string(), |ms| ms.to_string()),
                ));
            }
            Act::Healed { second_ms, .. } => driver.last_answer_ms = Some(second_ms),
            Act::Dropped(Dropped::Superseded) if reason == Reason::Debt => {
                w.superseded(sid, epoch);
            }
            Act::Dropped(_) => {}
            Act::GaveUp(why) => {
                if matches!(why, GiveUp::NoAnswer) {
                    w.unrepaired(sid, epoch);
                }
                w.give_up(sid, epoch, why);
            }
        }
        if let Some(end) = end_line(act, was, &driver.machine) {
            w.log(format!(
                "repaint nudge session={sid} reason={} epoch={epoch} {end}",
                reason.word()
            ));
        }
        let finished = matches!(driver.machine.phase(), Phase::Rest | Phase::Idle);
        let phase = driver.machine.phase();
        let nudged = driver.machine.nudged();
        if finished {
            driver.guard = None;
            driver.wake_ms = None;
            if self.pty_short.contains_key(&sid) {
                self.reconcile_one(w, sid);
            }
        }
        if matches!(
            act,
            Act::Healed { .. } | Act::GaveUp(_) | Act::Dropped(_) | Act::WouldNudge
        ) {
            w.publish(sid);
        }
        let driver = self.drivers.get_mut(&sid)?;
        if driver.stamped != (phase, nudged) {
            driver.stamped = (phase, nudged);
            w.stamp(
                sid,
                if phase == Phase::Rest {
                    "idle"
                } else {
                    phase.wire()
                },
                nudged,
            );
        }
        driver.wake_ms
    }
}

fn end_line(act: Act, was: Phase, machine: &RepaintNudge) -> Option<String> {
    let ms = |at: Option<u64>| at.map_or_else(|| "-".to_string(), |ms| ms.to_string());
    let held = matches!(was, Phase::Short | Phase::Restoring);
    match act {
        Act::Healed {
            first_ms,
            second_ms,
        } => Some(format!("healed by=nudge answer_ms={first_ms},{second_ms}")),
        Act::GaveUp(why) => Some(format!("gave-up why={}", why.word())),
        Act::Dropped(Dropped::Superseded) => Some("superseded by a real resize".to_string()),
        Act::Dropped(Dropped::NothingOwed) if held => Some(format!(
            "ended unanswered with nothing owed first_answer_ms={}",
            ms(machine.first_answer_ms())
        )),
        Act::Dropped(Dropped::Repainted) => Some(format!(
            "repainted with no first answer answer_ms=-,{}",
            ms(machine.second_answer_ms())
        )),
        Act::Release if machine.phase() == Phase::Rest => Some(
            "ended at the release: the alternate screen was left, so no debt of it is left to \
             repair"
                .to_string(),
        ),
        Act::Dropped(
            Dropped::Off
            | Dropped::NotAgent
            | Dropped::LeftAltScreen
            | Dropped::NothingOwed
            | Dropped::TooSmall,
        )
        | Act::Release
        | Act::Nothing
        | Act::WaitUntil(_)
        | Act::Hold
        | Act::Forget
        | Act::WouldNudge => None,
    }
}

/// The production world: the `App`'s sessions.
pub(crate) struct AppWorld<'a> {
    pub(crate) app: &'a crate::App,
    /// `(session, epoch)` pairs the step gave up on ([`crate::App::note_render_unrepaired`]).
    pub(crate) gave_up: Vec<(u64, u64, GiveUp)>,
}

impl<'a> AppWorld<'a> {
    /// A world over `app` with nothing given up on yet.
    pub(crate) const fn new(app: &'a crate::App) -> Self {
        Self {
            app,
            gave_up: Vec::new(),
        }
    }
}

impl NudgeWorld for AppWorld<'_> {
    fn now_ms(&self) -> u64 {
        crate::turn_ledger::now_ms()
    }

    fn alive(&self, session: u64) -> bool {
        self.app.pool.get(session).is_some()
    }

    fn attached(&self, session: u64) -> bool {
        self.app
            .pool
            .get(session)
            .is_some_and(|s| s.reader_join.is_some())
    }

    fn facts(&mut self, session: u64) -> Option<WorldFacts> {
        // The release is proven by reading the kernel's size back
        // (`TIOCGWINSZ`, [`aterm_pty::size`]); a platform whose PTY cannot be
        // read (ConPTY answers `None` always) has no proof that a held PTY
        // was put back, so it holds nothing.
        if !cfg!(unix) {
            return None;
        }
        let app = self.app;
        let s = app.pool.get(session)?;
        let now_us = crate::metrics::now_us();
        let (alt_screen, owed, selection, frame_open, clears, replaced, engine, cell_px) = {
            let term = crate::term_try_lock(&s.term)?;
            (
                term.modes().alternate_screen,
                term.render_debt().owed(),
                term.text_selection().has_selection(),
                term.modes().synchronized_output,
                term.full_clear_count(),
                term.screen_replaced_count(),
                (term.rows(), term.cols()),
                term.host_cell_pixel_size(),
            )
        };
        let agent = s
            .ctx
            .timeline
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .resize_agent_class();
        let driver_may_type = s
            .ctx
            .turn_lease
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            .is_some_and(|lease| lease.driver_may_type(now_us));
        let dragging = app
            .window_of_session(session)
            .and_then(|w| app.windows.get(&w))
            .is_some_and(|ws| ws.pending_resize.is_some());
        let now_ns = u64::try_from(
            std::time::Instant::now()
                .saturating_duration_since(app.lat_epoch)
                .as_nanos(),
        )
        .unwrap_or(u64::MAX);
        let last_ns = s
            .latest_output_activity_ns
            .load(std::sync::atomic::Ordering::Acquire);
        let output_quiet_ms = if last_ns == 0 {
            u64::MAX
        } else {
            now_ns.saturating_sub(last_ns) / 1_000_000
        };
        let pty = aterm_pty::size(s.master).unwrap_or(engine);
        let mut facts = Facts::eligible(Mode::default(), engine);
        facts.agent = agent;
        facts.alt_screen = alt_screen;
        facts.owed = owed;
        facts.shared = app.pool.views(session).is_some_and(|views| views > 1);
        facts.frozen = app.update_handoff_parked() || app.incoming_handoff_pending;
        facts.dragging = dragging;
        facts.halted = s.ctx.fabric.hold().is_some();
        facts.driver_may_type = driver_may_type;
        facts.selection = selection;
        facts.output_quiet_ms = output_quiet_ms;
        facts.frame_open = frame_open;
        facts.full_clears = clears;
        facts.screen_replaced = replaced;
        facts.pty = pty;
        Some(WorldFacts {
            facts,
            gesture_ms: s.ctx.human_input.ms_since(now_us),
            cell_px,
        })
    }

    fn pty_dims(&self, session: u64) -> Option<(u16, u16)> {
        aterm_pty::size(self.app.pool.get(session)?.master)
    }

    fn set_pty(&mut self, session: u64, rows: u16, cols: u16, cell_px: Option<(u16, u16)>) {
        if let Some(s) = self.app.pool.get(session) {
            aterm_pty::resize_with_cell_px(s.master, rows, cols, cell_px);
        }
    }

    fn restore_guard(
        &self,
        session: u64,
        fallback: (u16, u16),
        cell_px: Option<(u16, u16)>,
    ) -> PtyGuard {
        let Some(s) = self.app.pool.get(session) else {
            return PtyGuard::new(|| {});
        };
        let sink = std::sync::Arc::clone(&s.ctx.sink);
        let term = std::sync::Arc::downgrade(&s.term);
        PtyGuard::new(move || {
            let engine = term.upgrade().map_or(fallback, |term| {
                term.try_lock().map_or(fallback, |t| (t.rows(), t.cols()))
            });
            aterm_pty::resize_with_cell_px(sink.master(), engine.0, engine.1, cell_px);
        })
    }

    fn log(&mut self, line: String) {
        aterm_log::info!("{line}");
    }

    fn mark_tried(&mut self, session: u64, epoch: u64) {
        if let Some(s) = self.app.pool.get(session) {
            s.ctx
                .timeline
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .note_resize_tried(epoch);
        }
    }

    fn stamp(&mut self, session: u64, phase: &'static str, nudged: u32) {
        if let Some(s) = self.app.pool.get(session) {
            s.ctx
                .timeline
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .set_resize_nudge(phase, nudged);
        }
    }

    fn publish(&mut self, session: u64) {
        let app = self.app;
        let Some(s) = app.pool.get(session) else {
            return;
        };
        let Ok(term) = s.term.try_lock() else { return };
        let probe = crate::resize_ledger::EngineProbe::sample(&term);
        drop(term);
        let _ = s
            .ctx
            .timeline
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .evaluate_resizes(probe, crate::turn_ledger::now_ms());
    }

    fn give_up(&mut self, session: u64, epoch: u64, why: GiveUp) {
        self.gave_up.push((session, epoch, why));
    }

    fn unrepaired(&mut self, session: u64, epoch: u64) {
        if let Some(s) = self.app.pool.get(session) {
            s.ctx
                .timeline
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .note_resize_unrepaired(epoch);
        }
    }

    fn superseded(&mut self, session: u64, epoch: u64) {
        if let Some(s) = self.app.pool.get(session) {
            s.ctx
                .timeline
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .note_resize_superseded(epoch);
        }
    }

    fn epoch_view(&self, session: u64) -> Option<NudgeView> {
        self.app
            .pool
            .get(session)?
            .ctx
            .timeline
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .resize_nudge_view()
    }
}

impl crate::App {
    fn arm_nudges(&mut self) {
        let generation = crate::resize_epoch::nudge_due_generation();
        if self.nudges.mode() == Mode::Off || generation == self.nudge_gen_seen {
            return;
        }
        self.nudge_gen_seen = generation;
        let mut returned: Vec<(u64, u64, GiveUp)> = Vec::new();
        let candidates: Vec<(u64, u64)> = self
            .pool
            .iter()
            .filter_map(|s| {
                let mut timeline = s.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
                if let Some(epoch) = timeline.take_resize_returned() {
                    returned.push((s.id, epoch, GiveUp::NoAnswer));
                }
                let view = timeline.resize_nudge_view()?;
                Some((s.id, view.epoch))
            })
            .collect();
        for (session, epoch, why) in &returned {
            aterm_log::info!(
                "repaint nudge session={session} reason=debt epoch={epoch} gave-up why={}: a \
                 real resize superseded it and came back where the hold found it",
                why.word()
            );
        }
        let mut host = std::mem::take(&mut self.nudges);
        let mut world = AppWorld::new(self);
        for (session, epoch) in candidates {
            host.arm(&mut world, session, Reason::Debt, epoch);
        }
        let mut gave_up = world.gave_up;
        gave_up.append(&mut returned);
        self.nudges = host;
        self.raise_render_give_ups(gave_up);
    }

    fn raise_render_give_ups(&mut self, gave_up: Vec<(u64, u64, GiveUp)>) {
        for (session, epoch, why) in gave_up {
            self.note_render_unrepaired(session, epoch, why);
        }
    }

    /// Run the repaint nudge one step.
    pub(crate) fn tick_nudges(&mut self) {
        if repaint_adoptions_pending() {
            for session in take_repaint_adoptions() {
                self.nudges.watch_adoption(session);
            }
        }
        self.arm_nudges();
        self.check_render_repairs();
        if self.nudges.is_idle() {
            self.nudge_wake_ms = None;
            return;
        }
        let mut host = std::mem::take(&mut self.nudges);
        let mut world = AppWorld::new(self);
        let next = host.tick(&mut world);
        let gave_up = world.gave_up;
        self.nudges = host;
        self.nudge_wake_ms = next;
        self.raise_render_give_ups(gave_up);
    }

    /// Release every held PTY now (a handoff freeze is about to start).
    #[cfg(unix)]
    pub(crate) fn release_all_nudges(&mut self) {
        if self.nudges.is_idle() {
            return;
        }
        let mut host = std::mem::take(&mut self.nudges);
        let mut world = AppWorld::new(self);
        host.release_all(&mut world);
        let gave_up = world.gave_up;
        self.nudges = host;
        self.raise_render_give_ups(gave_up);
        self.nudge_wake_ms = None;
    }

    /// The instant the loop must wake for the nudge, if any.
    pub(crate) fn next_nudge_wake(&self) -> Option<std::time::Instant> {
        let at = self.nudge_wake_ms?;
        let now_ms = crate::turn_ledger::now_ms();
        Some(
            std::time::Instant::now() + std::time::Duration::from_millis(at.saturating_sub(now_ms)),
        )
    }
}

#[cfg(test)]
#[path = "app_nudge_tests.rs"]
mod tests;
