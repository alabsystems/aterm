// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 bind for `DsuQuiescence` (`aterm_spec::derive::dsu_quiescence_model`,
//! Rung 0 of `docs/RFC-proof-carrying-dsu.md`): an update may be applied only
//! at a quiescence point — never while a request is in flight.
//!
//! In the shipping overlap handoff a "request" is a slice of PTY output the
//! reader has taken OUT OF THE KERNEL and not yet folded into the engine, and
//! the "apply" is the handoff proceeding past `park_all_readers` to capture and
//! hand over the screens — which it does only when every `park_reader` returned
//! `true`. A capture taken with a slice in flight misses bytes that have already
//! left the kernel queue, so neither side of the handoff ever shows them: a
//! torn checkpoint. Quiescence is therefore a contract between two pieces of
//! code, the reader (which may stop only between slices) and `park_reader`
//! (which may ACK only once the reader has stopped), and both run here.
//!
//! The reader is the REAL one — `attach_reader_inner`, the gather + parse
//! pipeline every session runs — attached to a real pseudo-terminal. Only its
//! wakes are redirected (a unit test has no event loop). It is driven the way a
//! handoff meets it: output arrives, the reader drains it, and the engine is
//! busy. "Busy" is the test holding the session's terminal lock, the lock the
//! reader folds every slice under, so a slice the reader has drained stays in
//! flight until the test lets go.
//!
//! Projection, each variable read from the real pipeline:
//!
//! * `served`   — slices the ENGINE has applied: every slice is one printable
//!   byte, so this is the terminal's cursor column;
//! * `inflight` — the reader has drained more slices than the engine applied.
//!   A slice counts as drained once the reader's own post-drain activity stamp
//!   moved and the master has nothing left to read;
//! * `applied`  — `park_reader` returned `true` (the handoff may capture);
//! * `torn`     — it did so while a drained slice was still unapplied.
//!
//! The reader runs on its own threads, so one observation can cover several
//! slices; such a change is validated as the model path it has to be (`Finish`,
//! then `Begin`/`Finish` for each further slice), every hop tiered.
//!
//! NEGATIVE CONTROL: from a real in-flight state, an apply is rejected by the
//! committed model and admitted by `Buggy = 1`'s `BuggyApplyInflight`.

use std::collections::BTreeMap;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use aterm_core::terminal::Terminal;
use aterm_spec::derive::dsu_quiescence_model;
use aterm_spec::{interp, verify};

use super::{attach_reader_inner, park_reader};
use crate::{App, Session, Wake, WindowId};

type State = BTreeMap<&'static str, i64>;

/// `MaxReq` lifted past every real run.
const OVERRIDES: &[(&str, i64)] = &[("MaxReq", 1_000)];

/// Every wait in this file: generous, because a loaded machine only slows the
/// reader down — it never changes what the reader may do.
const PATIENCE: Duration = Duration::from_secs(10);

fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + PATIENCE;
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::yield_now();
    }
}

/// A real session on a real pseudo-terminal, with the real reader attached and
/// its wakes going to a channel instead of an event loop.
struct Rig {
    session: Session,
    slave: i32,
    /// Slices the reader has drained from the kernel.
    drained: i64,
}

impl Rig {
    fn attach(id: u64) -> Self {
        // The headless App only lends its `SessionFactory` to the attach.
        let app = App::headless_for_test();
        let (mut master, mut slave) = (-1i32, -1i32);
        // SAFETY: openpty(3) into two valid out-slots; no name/termios/winsize.
        let opened = unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        assert_eq!(opened, 0, "openpty");
        let mut session = crate::stub_session(id);
        session.master = master;
        let (ready_tx, ready_rx) = mpsc::channel::<()>();
        let post = std::sync::Arc::new(move |wake: Wake| {
            if matches!(wake, Wake::Ready { .. }) {
                let _ = ready_tx.send(());
            }
            true
        });
        attach_reader_inner(&mut session, WindowId(0), post, &app.session_factory, None)
            .expect("attach the real reader");
        ready_rx
            .recv_timeout(PATIENCE)
            .expect("the reader's gather started");
        Self {
            session,
            slave,
            drained: 0,
        }
    }

    /// The lock every slice is folded under; the test holding it is the engine
    /// being busy.
    fn term(&self) -> Arc<Mutex<Terminal>> {
        self.session.term.clone()
    }

    fn master_readable(&self) -> bool {
        let mut fd = libc::pollfd {
            fd: self.session.master,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one initialized pollfd; timeout 0 is a non-consuming peek.
        let polled = unsafe { libc::poll(&mut fd, 1, 0) };
        polled > 0 && fd.revents & libc::POLLIN != 0
    }

    /// The program writes one slice; wait until the reader has drained it.
    fn output_one_slice(&mut self) {
        let stamp = || {
            self.session
                .latest_output_activity_ns
                .load(Ordering::Acquire)
        };
        let before = stamp();
        // SAFETY: a one-byte write of a live slice to the test-owned slave.
        let wrote = unsafe { libc::write(self.slave, b"S".as_ptr().cast(), 1) };
        assert_eq!(wrote, 1, "write the slice");
        wait_for("the reader to drain the slice", || {
            stamp() != before && !self.master_readable()
        });
        self.drained += 1;
    }

    /// Slices the engine has applied, read from a terminal the caller holds.
    fn served(term: &Terminal) -> i64 {
        i64::from(term.cursor().col)
    }

    fn project(&self, served: i64, applied: bool) -> State {
        let inflight = self.drained > served;
        [
            ("inflight", i64::from(inflight)),
            ("applied", i64::from(applied)),
            ("torn", i64::from(applied && inflight)),
            ("served", served),
        ]
        .into_iter()
        .collect()
    }

    /// Project while holding the terminal (the reader cannot move under us).
    fn project_now(&self, applied: bool) -> State {
        let served = Self::served(&self.term().lock().expect("terminal lock"));
        self.project(served, applied)
    }

    /// Park the real reader; the apply happens iff the park ACKs.
    fn park(&mut self, budget: Duration) -> (bool, State) {
        let applied = park_reader(&mut self.session, Instant::now() + budget);
        (applied, self.project_now(applied))
    }

    fn reader_finished(&self) -> bool {
        self.session
            .reader_join
            .as_ref()
            .is_none_or(std::thread::JoinHandle::is_finished)
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        // The stub session owns no descriptor: close the pair here, once the
        // reader is gone (a park that failed leaves it running on `master`).
        let _ = park_reader(&mut self.session, Instant::now() + PATIENCE);
        let master = std::mem::replace(&mut self.session.master, -1);
        aterm_pty::close_fd(self.slave);
        aterm_pty::close_fd(master);
    }
}

fn conforms(prev: &State, next: &State, action: &str) -> (bool, String) {
    verify::validate_transition_tiered(
        &dsu_quiescence_model(),
        OVERRIDES,
        prev,
        next,
        Some(action),
        "DSU quiescence conformance",
    )
}

/// Validate an observed change of the reader's state as the model path it has
/// to be: `Finish` for the slice in flight, `Begin`/`Finish` for each further
/// slice applied, and a final `Begin` if one is in flight again. The
/// intermediate states are the model's own successors; every hop is tiered.
fn observe(prev: &State, next: &State) {
    if prev == next {
        return;
    }
    let finished = next["served"] - prev["served"];
    let mut path: Vec<&str> = Vec::new();
    if finished > 0 {
        path.push("Finish");
        for _ in 1..finished {
            path.extend(["Begin", "Finish"]);
        }
    }
    if next["inflight"] == 1 && (finished > 0 || prev["inflight"] == 0) {
        path.push("Begin");
    }
    assert!(!path.is_empty(), "{prev:?} -> {next:?} is no reader step");
    let model = interp::with_consts(&dsu_quiescence_model(), OVERRIDES);
    let mut cur = prev.clone();
    for (i, action) in path.iter().enumerate() {
        let hop = if i + 1 == path.len() {
            next.clone()
        } else {
            let successors = model.successors(action, &cur);
            assert_eq!(successors.len(), 1, "{action} from {cur:?}");
            successors[0].clone()
        };
        let (ok, why) = conforms(&cur, &hop, action);
        assert!(
            ok,
            "{prev:?} -> {next:?}: hop {cur:?} -> {hop:?} is not {action}\n{why}"
        );
        cur = hop;
    }
}

#[test]
fn the_real_reader_and_park_are_the_quiescence_gate() {
    let model = dsu_quiescence_model();
    let mut rig = Rig::attach(91);
    let mut state = rig.project_now(false);
    assert_eq!(
        state,
        model.init_state(),
        "an idle reader is the model's Init"
    );

    // One whole request: the reader drains a slice while the engine is busy,
    // then the engine applies it.
    let term = rig.term();
    let stalled = term.lock().expect("terminal lock");
    rig.output_one_slice();
    let next = rig.project(Rig::served(&stalled), false);
    observe(&state, &next);
    state = next;
    drop(stalled);
    wait_for("the engine to apply the slice", || {
        Rig::served(&term.lock().expect("terminal lock")) == state["served"] + 1
    });
    let next = rig.project_now(false);
    observe(&state, &next);
    state = next;

    // A slice is in flight, and more output queues behind it, when the handoff
    // asks to park: the gate must refuse, and nothing the reader drained may be
    // lost when it stops.
    let stalled = term.lock().expect("terminal lock");
    rig.output_one_slice();
    let next = rig.project(Rig::served(&stalled), false);
    observe(&state, &next);
    state = next;
    rig.output_one_slice();
    assert_eq!(
        rig.project(Rig::served(&stalled), false),
        state,
        "a queued slice behind the one in flight is no new model step"
    );
    assert!(!model.action_enabled("ApplyQuiescent", &state));
    let applied = park_reader(&mut rig.session, Instant::now() + Duration::from_millis(50));
    assert!(!applied, "the park must not authorize an apply mid-slice");
    assert!(
        rig.session.reader_join.is_some(),
        "a refused park keeps the handle for the retry"
    );
    assert_eq!(rig.project(Rig::served(&stalled), false), state);

    // The engine frees up; the stopped reader must finish EVERY slice it
    // drained before it exits, and only then may the park authorize the apply.
    drop(stalled);
    wait_for("the stopped reader to exit", || rig.reader_finished());
    let next = rig.project_now(false);
    observe(&state, &next);
    state = next;
    let (applied, next) = rig.park(PATIENCE);
    assert!(applied, "a quiescent reader parks");
    let (ok, why) = conforms(&state, &next, "ApplyQuiescent");
    assert!(ok, "apply: {state:?} -> {next:?}\n{why}");
    assert!(model.check_invariant("NoTear", &next));
    assert_eq!(
        next["served"], rig.drained,
        "every drained slice was applied"
    );
}

/// The defect from a REAL in-flight state: the apply lands mid-slice.
#[test]
fn an_apply_while_a_slice_is_in_flight_is_the_buggy_step() {
    let model = dsu_quiescence_model();
    let mut rig = Rig::attach(92);
    let idle = rig.project_now(false);
    let term = rig.term();
    let stalled = term.lock().expect("terminal lock");
    rig.output_one_slice();
    let inflight = rig.project(Rig::served(&stalled), false);
    assert!(conforms(&idle, &inflight, "Begin").0);

    let mut torn = inflight.clone();
    torn.insert("applied", 1);
    torn.insert("torn", 1);
    assert!(
        !conforms(&inflight, &torn, "BuggyApplyInflight").0,
        "the committed model must reject a mid-flight apply"
    );
    assert!(
        interp::with_consts(&interp::with_buggy(&model, 1), OVERRIDES)
            .successors("BuggyApplyInflight", &inflight)
            .contains(&torn),
        "Buggy = 1 admits exactly this tear"
    );
    assert!(!model.check_invariant("NoTear", &torn));
    drop(stalled);
}
