// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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
    fn unrepaired(&mut self, session: u64, epoch: u64) {
        self.s.borrow_mut().tried.push((session, epoch));
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
    w.s.borrow_mut().engine.insert(1, (62, 200));
    w.s.borrow_mut().pty.insert(1, (62, 200));
    w.advance(10);
    host.tick(&mut w);
    assert_eq!(host.held(), 0, "the hold is forgotten");
    assert_eq!(w.pty(1), (62, 200), "the layout's size stands");
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
    let (mut host, mut w) = armed(Mode::On);
    host.tick(&mut w);
    w.s.borrow_mut().halted = true;
    w.advance(10);
    host.tick(&mut w);
    assert_eq!(w.pty(1), ENGINE, "halt");
    assert_eq!(host.held(), 0);
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
    w.advance(PERSON_GATE_MS);
    let next = host.tick(&mut w).expect("a held PTY has a deadline");
    assert!(next <= w.now_ms() + PTY_HOLD_CAP_MS);
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
    host.drivers.remove(&1);
    assert_eq!(w.pty(1), ENGINE, "the guard's drop set the engine's size");
}

#[test]
fn a_hold_past_hold_cap_is_released_by_the_tick() {
    let (mut host, mut w) = armed(Mode::On);
    host.tick(&mut w);
    assert_ne!(w.pty(1), ENGINE, "held short");
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
    let next = host
        .tick(&mut w)
        .expect("an unconfirmed entry keeps a wake");
    assert!(next > w.now_ms(), "a future wake, not a past one");
    w.s.borrow_mut().blind = false;
    host.tick(&mut w);
    assert_eq!(w.pty(1), ENGINE);
    assert_eq!(host.held(), 0, "confirmed: the map is emptied");
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
    w.s.borrow_mut().frozen = true;
    for _ in 0..3 {
        w.advance(ANSWER_TIMEOUT_MS);
        host.tick(&mut w);
    }
    assert!(w.s.borrow().gave_up.is_empty(), "nothing judged frozen");
    assert_eq!(w.pty(1), ENGINE, "and nothing held again");
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
    assert!(!host.arm(&mut w, 1, Reason::Debt, 7));
}

#[test]
fn a_person_touching_the_session_defers_and_a_never_touched_one_waits_from_attach() {
    let mut w = Fake::new(&[1]);
    let mut host = NudgeHost::new(Mode::On);
    assert!(host.arm(&mut w, 1, Reason::Debt, 7));
    host.tick(&mut w);
    assert_eq!(w.pty(1), ENGINE, "held back by the never-stamped gate");
    w.advance(PERSON_GATE_MS);
    host.tick(&mut w);
    assert_ne!(w.pty(1), ENGINE);
}

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
    w.s.borrow_mut().attached = true;
    host.tick(&mut w);
    w.advance(ADOPTION_WAIT_MS - 1);
    host.tick(&mut w);
    assert!(w.s.borrow().logs.iter().all(|l| !l.contains("adoption")));
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
/// that PTY back at the size the hold recorded.
#[cfg(unix)]
#[test]
fn the_restore_guard_holds_the_master_past_its_session() {
    use std::os::fd::{FromRawFd, OwnedFd};
    use std::sync::Arc;

    use aterm_session::sink::SinkWriter;

    let (master, slave) = aterm_pty::open_pty_pair().expect("open_pty_pair");
    // SAFETY: Both are fresh descriptors this test alone owns; the sink takes the master.
    let (sink, slave) = unsafe {
        (
            Arc::new(SinkWriter::new_owned(OwnedFd::from_raw_fd(master))),
            OwnedFd::from_raw_fd(slave),
        )
    };
    let pinned = Arc::downgrade(&sink);
    let app = crate::App::headless_for_test_with_sink(sink);
    let engine = Arc::downgrade(&app.pool.get(0).expect("session 0").term);
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
