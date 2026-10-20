// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The repaint nudge, host side: the driver around the pure machine in
//! `aterm_core::terminal::repaint_nudge`. The machine says when the session's PTY is set one row
//! short ([`Act::Hold`]) and back ([`Act::Release`]); this file reads the world into [`Facts`],
//! performs those two PTY writes and keeps the release from being lost. It reaches the world only
//! through [`NudgeWorld`] ([`AppWorld`] in production, a table in the tests).
//!
//! The ordinary layout passes cannot repair a PTY left short (they write the PTY only when the
//! engine's size moves), so the release does not rest on this driver's own exit paths:
//!
//! 1. the hold is a [`PtyGuard`] whose `Drop` sets the PTY to the engine's current size;
//! 2. a hold has a hard deadline, [`PTY_HOLD_CAP_MS`], and the tick first releases any hold
//!    past it;
//! 3. [`NudgeHost::pty_short`] lives outside the drivers and is emptied only when `TIOCGWINSZ`
//!    reads the engine's size, so a hold whose driver vanished meets the same deadline;
//! 4. a handoff freeze calls [`NudgeHost::release_all`] first, and `Facts::frozen` is a gate;
//! 5. a window pass that changes the engine's size sets the PTY to its plan anyway: the machine
//!    forgets the hold ([`Act::Forget`], no write).
//!
//! An attempt that held always ends judged (healed, a give-up the owner hears, or a drop whose
//! reason is true) with one `aterm.log` line ([`end_line`]). A real resize that moves the engine
//! ends it [`Dropped::Superseded`]; if that resize comes back, the epoch judge raises the give-up
//! the attempt would have reached ([`NudgeWorld::superseded`], `crate::resize_epoch`).
//!
//! Armed by the epoch's judgement (`resize_epoch::note_nudge_due`), never by the status sweep;
//! ticked from `about_to_wait`, its next instant folded into the loop's deadline under
//! `DeadlineOwner::RenderEpoch`. `[render] repaint_nudge`: `on` acts; `log` runs everything but
//! the two PTY writes and logs `would-nudge ...`; `off` arms nothing. Only the agent class is
//! ever held.

use std::collections::HashMap;

use aterm_core::terminal::repaint_nudge::{
    Act, Dropped, Facts, GiveUp, Mode, PTY_HOLD_CAP_MS, Phase, Reason, RepaintNudge,
    person_quiet_ms,
};

use crate::resize_epoch::NudgeView;

/// The adoption arm's epoch id: an adoption is not one of the ledger's epochs
/// and is tried once per session.
pub(crate) const ADOPTION_EPOCH: u64 = u64::MAX;

/// The beat at which a held PTY that the reconcile could not clear is retried
/// once its hard deadline has passed.
const PTY_RETRY_MS: u64 = 250;

/// How long after its reader attached a repaint-adopted agent tab is given to
/// answer the adoption pulse with a full repaint before the nudge is armed.
pub(crate) const ADOPTION_WAIT_MS: u64 = 1_500;

/// How long the class of a repaint-adopted tab is waited for (the presence
/// classifier names a program by its foreground group, which an adopted session
/// has only after its first sweep). A program not known to be Claude Code by
/// then is left to the pulse alone.
pub(crate) const ADOPTION_CLASS_MS: u64 = 5_000;

/// Sessions adopted with a blank engine on purpose (`Adopted::repaint`), noted
/// by `spawn_session` and drained by the host's tick: a process-wide list, like
/// the epoch look's, because the spawn seam has no `App` to hand it to.
static REPAINT_ADOPTIONS: std::sync::Mutex<Vec<u64>> = std::sync::Mutex::new(Vec::new());

/// `spawn_session` adopted `session` with a blank screen its program is
/// expected to repaint.
#[cfg(unix)]
pub(crate) fn note_repaint_adoption(session: u64) {
    REPAINT_ADOPTIONS
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .push(session);
}

/// The sessions noted since the last call (the host's tick drains them).
fn take_repaint_adoptions() -> Vec<u64> {
    std::mem::take(&mut *REPAINT_ADOPTIONS.lock().unwrap_or_else(|p| p.into_inner()))
}

/// Whether any adoption waits to be taken into the watch.
fn repaint_adoptions_pending() -> bool {
    !REPAINT_ADOPTIONS
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .is_empty()
}

/// What a session's world says, minus the two facts only the host knows (the
/// mode and whether another session is held).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct WorldFacts {
    /// The machine's facts with `mode` and `other_hold` left at defaults and
    /// `person_quiet_ms` to be filled from [`Self::gesture_ms`].
    pub(crate) facts: Facts,
    /// Milliseconds since a person's last gesture on the session, `None`
    /// when none has reached it in this process.
    pub(crate) gesture_ms: Option<u64>,
    /// The host's cell size in device pixels, for the winsize pixel fields.
    pub(crate) cell_px: Option<(u16, u16)>,
}

/// A PTY held one row short, released on drop (see the module note).
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
    /// The timeline clock, in ms (monotonic).
    fn now_ms(&self) -> u64;
    /// Whether the session is still hosted.
    fn alive(&self, session: u64) -> bool;
    /// Whether the session's PTY reader is attached (an adopted session's
    /// reader is deferred until Commit; nothing it draws is seen before).
    fn attached(&self, session: u64) -> bool;
    /// The facts of the session's world now; `None` when the terminal is busy
    /// (asked again shortly) or the session is gone.
    fn facts(&mut self, session: u64) -> Option<WorldFacts>;
    /// The PTY's size as the kernel reports it, `None` when unreadable.
    fn pty_dims(&self, session: u64) -> Option<(u16, u16)>;
    /// Set the PTY's size (`TIOCSWINSZ`).
    fn set_pty(&mut self, session: u64, rows: u16, cols: u16, cell_px: Option<(u16, u16)>);
    /// A guard that sets the PTY to the engine's current size when dropped.
    fn restore_guard(
        &self,
        session: u64,
        fallback: (u16, u16),
        cell_px: Option<(u16, u16)>,
    ) -> PtyGuard;
    /// One `aterm.log` line.
    fn log(&mut self, line: String);
    /// The nudge held the PTY for epoch `epoch`: tried for good.
    fn mark_tried(&mut self, session: u64, epoch: u64);
    /// The machine's phase and attempt count, for the `resizes` header.
    fn stamp(&mut self, session: u64, phase: &'static str, nudged: u32);
    /// The attempt ended; let the ledger read the engine (an epoch the repaint
    /// healed closes `by=nudge`) and tell whoever watches.
    fn publish(&mut self, session: u64);
    /// The repair failed and the screen is probably still a row off. The production world hands
    /// it to the window after the step, which raises the `render.<tab>.unrepaired` digest line.
    fn give_up(&mut self, session: u64, epoch: u64, why: GiveUp);
    /// A real resize superseded the attempt for `epoch`: the epoch judge owes
    /// its end if the resize comes back (`crate::resize_epoch`).
    fn superseded(&mut self, session: u64, epoch: u64);
    /// The epoch's numbers for the log line.
    fn epoch_view(&self, session: u64) -> Option<NudgeView>;
}

struct NudgeDriver {
    machine: RepaintNudge,
    guard: Option<PtyGuard>,
    /// When this driver was made: the "attach" of a session that no person has
    /// touched in this process.
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

/// A repaint-adopted session waiting to be told apart: repainted by the
/// adoption pulse (a gain), or left blank by a program that will not (armed).
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
    /// Outside the drivers on purpose: see the module note, (3).
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

    /// Arm the nudge for `session`'s `epoch` (or an adoption). `true` when the
    /// machine took it.
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

    /// Let every hold go before a handoff freeze, so the successor adopts each PTY at the size
    /// its program believes. Each attempt then waits the freeze out as after a release (no parked
    /// reader carries an answer): a successor judges the debt it is handed, and after a rollback
    /// the attempt is judged as any other. An armed one waits at its freeze gate.
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

    /// The host lets `session`'s hold go (`why`): the guard's drop sets the
    /// PTY to the engine's size, the map entry goes once the kernel agrees
    /// (the reconcile), and the attempt waits for its answer as after a
    /// release ([`RepaintNudge::let_go`]), stepped at once. `resizes` says so
    /// now: the step that would stamp it may not read the world for a while
    /// (the deadline fires when no step could).
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

    /// One session's held PTY: set it to the engine's size and forget it when
    /// the kernel agrees. Returns whether the hold is gone.
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

    /// Run every driver one step and settle the held-PTY map. Returns the
    /// instant (timeline ms) the loop must call again, if any.
    pub(crate) fn tick<W: NudgeWorld>(&mut self, w: &mut W) -> Option<u64> {
        let now = w.now_ms();
        let mut next = self.tick_watch(w, now);
        // (2) and (3): a hold past its hard deadline, or whose driver is gone.
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
                // No step ended the hold (the loop did not tick, or the
                // terminal lock answered nothing at every look): the deadline
                // does, and the attempt is judged at the steps below.
                self.let_go(w, session, now, &format!("hold past {PTY_HOLD_CAP_MS} ms"));
            }
            // A driver finished or gone leaves the map entry as the only record.
            self.reconcile_one(w, session);
        }
        // Drivers of sessions that are gone.
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
            // A driver in flight is stepped at every tick (one `try_lock` and a
            // few loads): its gates end with an event the loop sees, and a
            // superseding resize or a halt must be seen at once, not at the
            // machine's next timer. A finished one only reports its deadline.
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
        // A held PTY has a deadline of its own. Past its cap, an entry the reconcile could not
        // clear (lock contended or poisoned, kernel did not confirm) is retried at a slow beat,
        // never at an instant already gone, which would re-arm an immediate wake every turn.
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

    /// Watch a session `spawn_session` adopted blank (mode `off` watches
    /// nothing): see [`Self::tick_watch`].
    pub(crate) fn watch_adoption(&mut self, session: u64) {
        if self.mode != Mode::Off {
            self.watch.entry(session).or_insert(Watch {
                since_ms: None,
                base: (0, 0),
            });
        }
    }

    /// Settle every watched adoption: repainted by the pulse (a gain), still blank and known to
    /// be Claude Code (armed once), or not Claude Code (left to the pulse). The clock runs from
    /// the reader's attach with no handoff freeze on, never from the spawn, which for a deferred
    /// reader is well before anything the program draws can be seen.
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
                // The attach (or the end of the freeze) is an event the loop sees.
                continue;
            }
            let since = *entry.since_ms.get_or_insert(now);
            if entry.since_ms == Some(now) {
                entry.base = (facts.full_clears, facts.screen_replaced);
            }
            if facts.full_clears > entry.base.0 || facts.screen_replaced > entry.base.1 {
                // The pulse (or the program) repainted it: a gain.
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
                // Not known to be Claude Code on the alternate screen this long after attach:
                // left to the pulse.
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
            // Busy terminal: ask again shortly.
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
                    // The kernel did not take it: the reconcile keeps trying.
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
                // The program was given a size of its own; if the resize comes
                // back after the wait, the epoch judge owes this attempt's end.
                w.superseded(sid, epoch);
            }
            Act::Dropped(_) => {}
            Act::GaveUp(why) => {
                if matches!(why, GiveUp::NoAnswer) {
                    w.mark_tried(sid, epoch);
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
        // Whatever ended the attempt, the PTY ends at the engine's size.
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
            // A finished machine is idle again; `nudged` keeps the count.
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

/// How the step's `act` ended the attempt, for its `aterm.log` line: every end
/// of an attempt that held and every give-up has one; `None` for an act that
/// ends nothing (the hold and its release have lines of their own) and for a
/// drop before any hold (nothing was tried). An exhaustive match, so a new end
/// is never silent by default.
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
        // The wait for the size that stands was spent, but the engine owes
        // nothing: nothing to report (the machine's module note).
        Act::Dropped(Dropped::NothingOwed) if held => Some(format!(
            "ended unanswered with nothing owed first_answer_ms={}",
            ms(machine.first_answer_ms())
        )),
        // The size that stands was answered with no first answer seen (a hold
        // someone else ended, or a short size left unanswered until the
        // release): repainted, nothing to report.
        Act::Dropped(Dropped::Repainted) => Some(format!(
            "repainted with no first answer answer_ms=-,{}",
            ms(machine.second_answer_ms())
        )),
        // The one release that ends the attempt (the machine's `step_short`).
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

// ---------------------------------------------------------------------------
// The App's half.
// ---------------------------------------------------------------------------

/// The production world: the `App`'s sessions.
pub(crate) struct AppWorld<'a> {
    pub(crate) app: &'a crate::App,
    /// The `(session, epoch)` pairs the step gave up on: the window raises the
    /// digest line once the step has returned the borrow of the app
    /// ([`crate::App::note_render_unrepaired`]).
    pub(crate) gave_up: Vec<(u64, u64, GiveUp)>,
}

impl<'a> AppWorld<'a> {
    /// A world over `app` with nothing given up on yet.
    pub(crate) fn new(app: &'a crate::App) -> Self {
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
            // A lock a panic poisoned is read as the rest of the window reads
            // it: `None` is for a busy terminal, asked again shortly.
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
        // The guard holds a sink clone, so the master stays open until it drops: a bare number
        // could name another tab's PTY by the next tick. The terminal is read only for its size
        // (the parse stage or a worker can outlive the master, so it proves nothing about it).
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
        let moved = s
            .ctx
            .timeline
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .evaluate_resizes(probe, crate::turn_ledger::now_ms());
        if moved && app.subscribers.any() {
            app.subscribers
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .notify(session);
        }
    }

    fn give_up(&mut self, session: u64, epoch: u64, why: GiveUp) {
        self.gave_up.push((session, epoch, why));
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
    /// Arm the nudge for every untried agent-class epoch at `desync-risk`, and raise the give-up
    /// owed for a superseded attempt whose resize came back. One atomic load when nothing changed.
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

    /// The digest lines for the repairs a step gave up on.
    fn raise_render_give_ups(&mut self, gave_up: Vec<(u64, u64, GiveUp)>) {
        for (session, epoch, why) in gave_up {
            self.note_render_unrepaired(session, epoch, why);
        }
    }

    /// Run the repaint nudge one step (see the module note). Called from the
    /// event loop's `about_to_wait`, right after the epoch look.
    pub(crate) fn tick_nudges(&mut self) {
        if repaint_adoptions_pending() {
            for session in take_repaint_adoptions() {
                self.nudges.watch_adoption(session);
            }
        }
        self.arm_nudges();
        // A repair that gave up is a line of the digest until its epoch heals.
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
mod tests {
    use std::cell::RefCell;
    use std::collections::BTreeMap;
    use std::rc::Rc;

    use aterm_core::terminal::repaint_nudge::{ANSWER_TIMEOUT_MS, PERSON_GATE_MS, SETTLE_MS};

    use super::*;

    const ENGINE: (u16, u16) = (61, 200);

    #[derive(Default)]
    struct Shared {
        engine: BTreeMap<u64, (u16, u16)>,
        pty: BTreeMap<u64, (u16, u16)>,
        writes: Vec<(u64, u16, u16)>,
        logs: Vec<String>,
        tried: Vec<(u64, u64)>,
        stamps: Vec<(u64, &'static str, u32)>,
        published: Vec<u64>,
        gave_up: Vec<(u64, u64, GiveUp)>,
        superseded: Vec<(u64, u64)>,
        clears: BTreeMap<u64, u64>,
        halted: bool,
        /// The program is Claude Code (the class the ledger reads).
        agent: bool,
        /// The reader is attached.
        attached: bool,
        frozen: bool,
        /// The terminal lock is busy or poisoned: `facts` answers nothing.
        blind: bool,
        now: u64,
    }

    /// A world of scripted sessions; the program answers when told.
    struct Fake {
        s: Rc<RefCell<Shared>>,
        /// A `set_pty` that does nothing (a kernel that refuses).
        deaf: bool,
    }

    impl Fake {
        fn new(sessions: &[u64]) -> Self {
            let s = Rc::new(RefCell::new(Shared::default()));
            for id in sessions {
                s.borrow_mut().engine.insert(*id, ENGINE);
                s.borrow_mut().pty.insert(*id, ENGINE);
                s.borrow_mut().clears.insert(*id, 0);
            }
            s.borrow_mut().now = 1_000_000;
            s.borrow_mut().agent = true;
            s.borrow_mut().attached = true;
            Self { s, deaf: false }
        }
        fn advance(&self, ms: u64) {
            self.s.borrow_mut().now += ms;
        }
        fn answer(&self, id: u64) {
            *self.s.borrow_mut().clears.get_mut(&id).unwrap() += 1;
        }
        fn pty(&self, id: u64) -> (u16, u16) {
            self.s.borrow().pty[&id]
        }
    }

    impl NudgeWorld for Fake {
        fn now_ms(&self) -> u64 {
            self.s.borrow().now
        }
        fn alive(&self, session: u64) -> bool {
            self.s.borrow().engine.contains_key(&session)
        }
        fn attached(&self, _session: u64) -> bool {
            self.s.borrow().attached
        }
        fn facts(&mut self, session: u64) -> Option<WorldFacts> {
            let s = self.s.borrow();
            if s.blind {
                return None;
            }
            let engine = *s.engine.get(&session)?;
            let mut f = Facts::eligible(Mode::On, engine);
            f.halted = s.halted;
            f.agent = s.agent;
            f.frozen = s.frozen;
            f.full_clears = s.clears[&session];
            f.pty = s.pty[&session];
            Some(WorldFacts {
                facts: f,
                gesture_ms: None,
                cell_px: Some((9, 17)),
            })
        }
        fn pty_dims(&self, session: u64) -> Option<(u16, u16)> {
            self.s.borrow().pty.get(&session).copied()
        }
        fn set_pty(&mut self, session: u64, rows: u16, cols: u16, _cell_px: Option<(u16, u16)>) {
            if self.deaf {
                return;
            }
            let mut s = self.s.borrow_mut();
            s.writes.push((session, rows, cols));
            s.pty.insert(session, (rows, cols));
        }
        fn restore_guard(
            &self,
            session: u64,
            fallback: (u16, u16),
            _cell_px: Option<(u16, u16)>,
        ) -> PtyGuard {
            let shared = Rc::clone(&self.s);
            PtyGuard::new(move || {
                let mut s = shared.borrow_mut();
                let engine = s.engine.get(&session).copied().unwrap_or(fallback);
                s.writes.push((session, engine.0, engine.1));
                s.pty.insert(session, engine);
            })
        }
        fn log(&mut self, line: String) {
            self.s.borrow_mut().logs.push(line);
        }
        fn mark_tried(&mut self, session: u64, epoch: u64) {
            self.s.borrow_mut().tried.push((session, epoch));
        }
        fn stamp(&mut self, session: u64, phase: &'static str, nudged: u32) {
            self.s.borrow_mut().stamps.push((session, phase, nudged));
        }
        fn publish(&mut self, session: u64) {
            self.s.borrow_mut().published.push(session);
        }
        fn give_up(&mut self, session: u64, epoch: u64, why: GiveUp) {
            self.s.borrow_mut().gave_up.push((session, epoch, why));
        }
        fn superseded(&mut self, session: u64, epoch: u64) {
            self.s.borrow_mut().superseded.push((session, epoch));
        }
        fn epoch_view(&self, _session: u64) -> Option<NudgeView> {
            Some(NudgeView {
                epoch: 7,
                shift: -1,
                pushed: 0,
                cut: 0,
                drew: true,
            })
        }
    }

    /// A host with session 1 armed and past every gate (the fake reports a
    /// session no person ever touched, which waits from attach).
    fn armed(mode: Mode) -> (NudgeHost, Fake) {
        let mut w = Fake::new(&[1, 2]);
        let mut host = NudgeHost::new(mode);
        assert!(host.arm(&mut w, 1, Reason::Debt, 7));
        w.advance(PERSON_GATE_MS);
        (host, w)
    }

    #[test]
    fn the_hold_sets_the_pty_one_row_short_and_the_release_restores_the_engine_dims() {
        let (mut host, mut w) = armed(Mode::On);
        host.tick(&mut w);
        assert_eq!(w.pty(1), (ENGINE.0 - 1, ENGINE.1), "held one row short");
        assert_eq!(host.held(), 1);
        assert_eq!(w.s.borrow().tried, [(1, 7)], "tried for good at the hold");
        w.advance(5);
        w.answer(1);
        w.advance(SETTLE_MS);
        host.tick(&mut w);
        assert_eq!(w.pty(1), ENGINE, "released to the ENGINE's size");
        assert_eq!(host.held(), 0);
        w.advance(5);
        w.answer(1);
        w.advance(SETTLE_MS);
        host.tick(&mut w);
        assert!(
            w.s.borrow()
                .logs
                .iter()
                .any(|l| l.contains("healed by=nudge")),
            "{:?}",
            w.s.borrow().logs
        );
        assert_eq!(w.pty(1), ENGINE);
        assert_eq!(
            w.s.borrow().engine[&1],
            ENGINE,
            "the engine was never asked"
        );
        assert_eq!(w.s.borrow().published, [1]);
    }

    #[test]
    fn mode_off_arms_nothing() {
        let mut w = Fake::new(&[1]);
        let mut host = NudgeHost::new(Mode::Off);
        assert!(!host.arm(&mut w, 1, Reason::Debt, 7));
        assert!(host.is_idle());
        assert_eq!(host.tick(&mut w), None);
    }

    #[test]
    fn a_real_resize_meanwhile_supersedes_the_hold() {
        let (mut host, mut w) = armed(Mode::On);
        host.tick(&mut w);
        assert_eq!(host.held(), 1);
        // The window resized the engine; the layout pass set the PTY to its plan.
        w.s.borrow_mut().engine.insert(1, (62, 200));
        w.s.borrow_mut().pty.insert(1, (62, 200));
        w.advance(10);
        host.tick(&mut w);
        assert_eq!(host.held(), 0, "the hold is forgotten");
        assert_eq!(w.pty(1), (62, 200), "the layout's size stands");
        // Unanswered when the wait ends: the program was given a size of its
        // own, so the attempt is superseded, never a give-up.
        w.advance(ANSWER_TIMEOUT_MS);
        host.tick(&mut w);
        assert_eq!(w.pty(1), (62, 200));
        assert!(w.s.borrow().logs.iter().any(|l| l.contains("superseded")));
        assert!(w.s.borrow().gave_up.is_empty());
        assert_eq!(w.s.borrow().superseded, [(1, 7)], "the epoch judge is told");
        assert!(host.is_idle());
    }

    #[test]
    fn every_exit_path_lifts_the_hold() {
        // A halt during the hold.
        let (mut host, mut w) = armed(Mode::On);
        host.tick(&mut w);
        w.s.borrow_mut().halted = true;
        w.advance(10);
        host.tick(&mut w);
        assert_eq!(w.pty(1), ENGINE, "halt");
        assert_eq!(host.held(), 0);
        // No answer at all.
        let (mut host, mut w) = armed(Mode::On);
        host.tick(&mut w);
        w.advance(ANSWER_TIMEOUT_MS);
        host.tick(&mut w);
        assert_eq!(w.pty(1), ENGINE, "timeout releases");
        w.advance(ANSWER_TIMEOUT_MS);
        host.tick(&mut w);
        assert_eq!(w.pty(1), ENGINE);
        assert_eq!(host.held(), 0);
        assert!(
            w.s.borrow()
                .logs
                .iter()
                .any(|l| l.contains("gave-up why=no-answer"))
        );
        // The session goes away.
        let (mut host, mut w) = armed(Mode::On);
        host.tick(&mut w);
        w.s.borrow_mut().engine.remove(&1);
        host.tick(&mut w);
        assert_eq!(host.held(), 0);
        assert!(host.is_idle());
    }

    #[test]
    fn an_armed_nudge_arms_a_deadline_and_an_idle_window_arms_none() {
        let mut w = Fake::new(&[1]);
        let mut host = NudgeHost::new(Mode::On);
        assert!(host.is_idle());
        assert_eq!(host.tick(&mut w), None, "nothing armed, nothing folded");
        assert!(host.arm(&mut w, 1, Reason::Debt, 7));
        assert!(!host.is_idle());
        // Held: the deadline is the hold's own hard cap at the latest.
        w.advance(PERSON_GATE_MS);
        let next = host.tick(&mut w).expect("a held PTY has a deadline");
        assert!(next <= w.now_ms() + PTY_HOLD_CAP_MS);
        // Finished: nothing folded again.
        w.advance(ANSWER_TIMEOUT_MS);
        host.tick(&mut w);
        w.advance(ANSWER_TIMEOUT_MS);
        host.tick(&mut w);
        assert_eq!(host.tick(&mut w), None);
        assert!(host.is_idle());
    }

    #[test]
    fn a_dropped_hold_restores_the_pty_on_drop() {
        let (mut host, mut w) = armed(Mode::On);
        host.tick(&mut w);
        assert_ne!(w.pty(1), ENGINE);
        // A panic unwinding the tick, a teardown race: the driver is dropped
        // with its hold.
        host.drivers.remove(&1);
        assert_eq!(w.pty(1), ENGINE, "the guard's drop set the engine's size");
    }

    #[test]
    fn a_hold_past_hold_cap_is_released_by_the_tick() {
        let (mut host, mut w) = armed(Mode::On);
        host.tick(&mut w);
        assert_ne!(w.pty(1), ENGINE, "held short");
        // The machine is not stepped at all (the terminal lock is busy), so
        // none of its own timeouts can release the hold: only the cap does.
        w.s.borrow_mut().blind = true;
        w.advance(PTY_HOLD_CAP_MS - 1);
        host.tick(&mut w);
        assert_ne!(w.pty(1), ENGINE, "one millisecond short of the cap");
        assert_eq!(host.held(), 1);
        w.advance(1);
        host.tick(&mut w);
        assert_eq!(w.pty(1), ENGINE, "the cap released the hold");
        assert_eq!(
            w.s.borrow().stamps.last(),
            Some(&(1, "restoring", 1)),
            "`resizes` says the hold is over, though no step can read the world"
        );
        // The map entry stays until the kernel's size is confirmed against the
        // engine's facts, and it never asks the loop for an instant gone.
        let next = host
            .tick(&mut w)
            .expect("an unconfirmed entry keeps a wake");
        assert!(next > w.now_ms(), "a future wake, not a past one");
        w.s.borrow_mut().blind = false;
        host.tick(&mut w);
        assert_eq!(w.pty(1), ENGINE);
        assert_eq!(host.held(), 0, "confirmed: the map is emptied");
        // The deadline let the hold go; the attempt, its one try spent at the
        // hold, is judged as after any release: unanswered on a tab that still
        // owes rows, the give-up the owner hears, and `resizes` says it ended.
        w.advance(ANSWER_TIMEOUT_MS);
        host.tick(&mut w);
        assert_eq!(w.s.borrow().gave_up, [(1, 7, GiveUp::NoAnswer)]);
        assert_eq!(w.s.borrow().stamps.last(), Some(&(1, "idle", 1)));
        assert!(host.is_idle());
    }

    #[test]
    fn a_lost_drivers_hold_is_found_and_released_by_the_reconcile() {
        let (mut host, mut w) = armed(Mode::On);
        host.tick(&mut w);
        // The driver vanishes without running its guard (a leaked guard): only
        // the app-level map remembers the hold.
        let driver = host.drivers.remove(&1).unwrap();
        std::mem::forget(driver);
        assert_ne!(w.pty(1), ENGINE);
        assert_eq!(host.held(), 1);
        host.tick(&mut w);
        assert_eq!(
            w.pty(1),
            ENGINE,
            "the map's entry has no driver: released now"
        );
        assert_eq!(host.held(), 0);
    }

    #[test]
    fn a_release_the_kernel_refuses_is_kept_in_the_map_until_it_lands() {
        let (mut host, mut w) = armed(Mode::On);
        host.tick(&mut w);
        w.deaf = true;
        w.advance(ANSWER_TIMEOUT_MS);
        host.tick(&mut w);
        assert_eq!(host.held(), 1, "not confirmed: still remembered");
        w.deaf = false;
        w.advance(10);
        host.tick(&mut w);
        assert_eq!(w.pty(1), ENGINE);
        assert_eq!(host.held(), 0);
    }

    #[test]
    fn a_handoff_freeze_releases_every_hold_before_it_starts() {
        let mut w = Fake::new(&[1, 2]);
        let mut host = NudgeHost::new(Mode::On);
        assert!(host.arm(&mut w, 1, Reason::Debt, 7));
        w.advance(PERSON_GATE_MS);
        host.tick(&mut w);
        assert_eq!(host.held(), 1);
        host.release_all(&mut w);
        assert_eq!(w.pty(1), ENGINE);
        assert_eq!(host.held(), 0);
        // The attempt, its one try spent at the hold, waits the freeze out: a
        // parked reader carries no answer, and nothing is judged under it.
        w.s.borrow_mut().frozen = true;
        for _ in 0..3 {
            w.advance(ANSWER_TIMEOUT_MS);
            host.tick(&mut w);
        }
        assert!(w.s.borrow().gave_up.is_empty(), "nothing judged frozen");
        assert_eq!(w.pty(1), ENGINE, "and nothing held again");
        // A rollback lifts the freeze: the wait starts again and the attempt is
        // judged as after any release.
        w.s.borrow_mut().frozen = false;
        host.tick(&mut w);
        assert!(w.s.borrow().gave_up.is_empty(), "the wait starts again");
        w.advance(ANSWER_TIMEOUT_MS);
        host.tick(&mut w);
        assert_eq!(w.s.borrow().gave_up, [(1, 7, GiveUp::NoAnswer)]);
        assert!(host.is_idle());
    }

    #[test]
    fn at_most_one_hold_app_wide_across_two_sessions() {
        let mut w = Fake::new(&[1, 2]);
        let mut host = NudgeHost::new(Mode::On);
        assert!(host.arm(&mut w, 1, Reason::Debt, 7));
        assert!(host.arm(&mut w, 2, Reason::Debt, 8));
        w.advance(PERSON_GATE_MS);
        host.tick(&mut w);
        assert_eq!(host.held(), 1, "the second waits its turn");
        let held = *host.pty_short.keys().next().unwrap();
        let other = if held == 1 { 2 } else { 1 };
        assert_eq!(w.pty(other), ENGINE);
    }

    #[test]
    fn a_give_up_posts_one_digest_item_per_epoch() {
        let (mut host, mut w) = armed(Mode::On);
        host.tick(&mut w);
        w.advance(ANSWER_TIMEOUT_MS);
        host.tick(&mut w);
        w.advance(ANSWER_TIMEOUT_MS);
        host.tick(&mut w);
        for _ in 0..3 {
            w.advance(1_000);
            host.tick(&mut w);
        }
        assert_eq!(w.s.borrow().gave_up, [(1, 7, GiveUp::NoAnswer)]);
        // The same epoch does not come back.
        assert!(!host.arm(&mut w, 1, Reason::Debt, 7));
    }

    #[test]
    fn a_person_touching_the_session_defers_and_a_never_touched_one_waits_from_attach() {
        let mut w = Fake::new(&[1]);
        let mut host = NudgeHost::new(Mode::On);
        assert!(host.arm(&mut w, 1, Reason::Debt, 7));
        // Never touched: waits PERSON_GATE_MS from the driver's creation.
        host.tick(&mut w);
        assert_eq!(w.pty(1), ENGINE, "held back by the never-stamped gate");
        w.advance(PERSON_GATE_MS);
        host.tick(&mut w);
        assert_ne!(w.pty(1), ENGINE);
    }

    /// A blank agent tab with no full repaint within `ADOPTION_WAIT_MS` of attach is armed once,
    /// then repaired through the same gates and bounds as a debt.
    #[test]
    fn a_blank_agent_tab_with_no_answer_in_1500ms_is_armed_once() {
        let mut w = Fake::new(&[1]);
        let mut host = NudgeHost::new(Mode::On);
        host.watch_adoption(1);
        host.tick(&mut w);
        assert_eq!(w.pty(1), ENGINE, "nothing yet");
        w.advance(ADOPTION_WAIT_MS - 1);
        host.tick(&mut w);
        assert_eq!(host.held(), 0, "still inside the pulse's window");
        w.advance(1);
        host.tick(&mut w);
        assert!(
            w.s.borrow()
                .logs
                .iter()
                .any(|l| l.contains("reason=adoption armed")),
            "{:?}",
            w.s.borrow().logs
        );
        // Armed, it waits out the never-touched gate like any nudge, then holds.
        w.advance(PERSON_GATE_MS);
        host.tick(&mut w);
        assert_ne!(w.pty(1), ENGINE, "held one row short");
        assert_eq!(host.held(), 1);
        w.advance(5);
        w.answer(1);
        w.advance(SETTLE_MS);
        host.tick(&mut w);
        w.advance(5);
        w.answer(1);
        w.advance(SETTLE_MS);
        host.tick(&mut w);
        assert_eq!(w.pty(1), ENGINE);
        assert!(
            w.s.borrow()
                .logs
                .iter()
                .any(|l| l.contains("reason=adoption") && l.contains("healed by=nudge"))
        );
        // Once: the same adoption is not armed again.
        host.watch_adoption(1);
        for _ in 0..8 {
            w.advance(ADOPTION_WAIT_MS);
            host.tick(&mut w);
        }
        assert_eq!(
            w.s.borrow().tried.len(),
            1,
            "one attempt: {:?}",
            w.s.borrow().tried
        );
    }

    /// Negative control: a blank tab whose program is not Claude Code is never held.
    #[test]
    fn a_blank_non_agent_tab_is_left_to_the_pulse() {
        let mut w = Fake::new(&[1]);
        w.s.borrow_mut().agent = false;
        let mut host = NudgeHost::new(Mode::On);
        host.watch_adoption(1);
        for _ in 0..40 {
            host.tick(&mut w);
            w.advance(250);
        }
        assert!(w.s.borrow().writes.is_empty(), "{:?}", w.s.borrow().writes);
        assert!(host.is_idle(), "the watch gave up on it");
    }

    /// Negative control: a program that answers the pulse (vim, less) is never nudged.
    #[test]
    fn an_adoption_the_pulse_repainted_is_never_nudged() {
        let mut w = Fake::new(&[1]);
        let mut host = NudgeHost::new(Mode::On);
        host.watch_adoption(1);
        host.tick(&mut w);
        w.advance(300);
        w.answer(1);
        for _ in 0..20 {
            host.tick(&mut w);
            w.advance(250);
        }
        assert!(w.s.borrow().writes.is_empty());
        assert!(host.is_idle());
    }

    /// The adoption wait counts from the reader's attach with no handoff freeze on, never from
    /// the spawn: a deferred reader's program cannot have answered before it.
    #[test]
    fn a_deferred_reader_adoption_arms_at_ready() {
        let mut w = Fake::new(&[1]);
        w.s.borrow_mut().attached = false;
        let mut host = NudgeHost::new(Mode::On);
        host.watch_adoption(1);
        for _ in 0..30 {
            host.tick(&mut w);
            w.advance(500);
        }
        assert!(
            w.s.borrow().logs.iter().all(|l| !l.contains("adoption")),
            "armed before the reader attached: {:?}",
            w.s.borrow().logs
        );
        // The Commit lands: the reader attaches, and the clock starts.
        w.s.borrow_mut().attached = true;
        host.tick(&mut w);
        w.advance(ADOPTION_WAIT_MS - 1);
        host.tick(&mut w);
        assert!(w.s.borrow().logs.iter().all(|l| !l.contains("adoption")));
        // A freeze restarts it.
        w.s.borrow_mut().frozen = true;
        host.tick(&mut w);
        w.s.borrow_mut().frozen = false;
        w.advance(10);
        host.tick(&mut w);
        w.advance(ADOPTION_WAIT_MS - 1);
        host.tick(&mut w);
        assert!(w.s.borrow().logs.iter().all(|l| !l.contains("adoption")));
        w.advance(1);
        host.tick(&mut w);
        assert!(
            w.s.borrow()
                .logs
                .iter()
                .any(|l| l.contains("reason=adoption armed"))
        );
    }

    /// Mode `off` watches no adoption; mode `log` arms it and says what it
    /// would do.
    #[test]
    fn an_adoption_follows_the_mode() {
        let mut w = Fake::new(&[1]);
        let mut host = NudgeHost::new(Mode::Off);
        host.watch_adoption(1);
        assert!(host.is_idle());
        let mut host = NudgeHost::new(Mode::Log);
        host.watch_adoption(1);
        host.tick(&mut w);
        w.advance(ADOPTION_WAIT_MS);
        host.tick(&mut w);
        w.advance(PERSON_GATE_MS);
        host.tick(&mut w);
        assert!(w.s.borrow().writes.is_empty(), "log mode holds nothing");
        assert!(
            w.s.borrow()
                .logs
                .iter()
                .any(|l| l.starts_with("would-nudge session=1 reason=adoption")),
            "{:?}",
            w.s.borrow().logs
        );
    }

    #[test]
    fn the_guard_runs_once_and_disarm_cancels_it() {
        let ran = Rc::new(RefCell::new(0));
        let r = Rc::clone(&ran);
        let g = PtyGuard::new(move || *r.borrow_mut() += 1);
        drop(g);
        assert_eq!(*ran.borrow(), 1);
        let r = Rc::clone(&ran);
        PtyGuard::new(move || *r.borrow_mut() += 1).disarm();
        assert_eq!(*ran.borrow(), 1);
    }

    /// The production guard holds the session's master, not its number: with the session and its
    /// terminal gone (a tab closed during a hold), the guard alone keeps the master open and puts
    /// that PTY back at the size the hold recorded. A captured number could by then name another
    /// tab's PTY.
    #[cfg(unix)]
    #[test]
    fn the_restore_guard_holds_the_master_past_its_session() {
        use std::os::fd::{FromRawFd, OwnedFd};
        use std::sync::Arc;

        use aterm_session::sink::SinkWriter;

        let (master, slave) = aterm_pty::open_pty_pair().expect("open_pty_pair");
        // SAFETY: both are fresh descriptors this test alone owns; the sink
        // takes the master.
        let (sink, slave) = unsafe {
            (
                Arc::new(SinkWriter::new_owned(OwnedFd::from_raw_fd(master))),
                OwnedFd::from_raw_fd(slave),
            )
        };
        let pinned = Arc::downgrade(&sink);
        let app = crate::App::headless_for_test_with_sink(sink);
        let engine = Arc::downgrade(&app.pool.get(0).expect("session 0").term);
        // The hold: one row short of the engine's 24 rows.
        aterm_pty::resize(master, 23, 80);
        let guard = AppWorld::new(&app).restore_guard(0, (24, 81), None);
        drop(app);
        assert_eq!(
            engine.strong_count(),
            0,
            "the terminal went with the session"
        );
        assert_eq!(
            pinned.strong_count(),
            1,
            "with its session gone, the guard alone holds the master"
        );
        let held = pinned.upgrade().expect("the guard's clone");
        drop(guard);
        assert_eq!(aterm_pty::size(held.master()), Some((24, 81)), "put back");
        drop(held);
        assert_eq!(
            pinned.strong_count(),
            0,
            "the master closes after the guard"
        );
        drop(slave);
    }
}
