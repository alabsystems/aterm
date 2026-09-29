// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance of the window show carry (`crate::window_show`) to
//! `aterm_spec::derive::native_update_window_show_model`
//! (`NativeUpdateWindowShow`, gap #29).
//!
//! Each schedule of the model is replayed on the SHIPPING App carry: a
//! headless App with the bootstrap window and two windows a rebuild created,
//! the real `CarriedShow` the rebuild records, and the four seams production
//! drives it through — `settle_carried_window_show` (what the event loop runs
//! after every event batch), the REAL proof path `maybe_signal_handoff_ready`
//! (over real ready and Commit pipes; it writes a real adoption proof, and it
//! is where the update lane's stack goes on), `commit_carried_window_show`
//! (the Commit arm) and `carried_window_show_focus_event` (the `Focused` arm,
//! after `on_focus`).
//! The test plays the OS: it reveals a window, activates and deactivates
//! aterm, moves the keyboard and types, and it delivers the focus events a raise
//! produces while aterm is in front — the way AppKit's `makeKeyAndOrderFront:`
//! does. Every real step is projected onto the model and checked: the
//! environment's move and every App action the real code took in the same
//! step, in order, each admitted by the committed model (interpreter, and `ty`
//! wherever installed). And the real code must be EAGER: after every step the
//! model may not still allow `Stack` or `EnterFullScreen` — a carry that
//! could act and did not is a stall the user sees as a window left wrong.
//!
//! NEGATIVE CONTROL: the same real steps against `Buggy = 1` — a stack before
//! the reveal or the proof, full screen at the stack, the reveal order's key, a
//! Commit that re-keys over typing — must be rejected, so a pass is never
//! vacuous.

use std::collections::BTreeMap;
use std::io::Read as _;
use std::os::fd::FromRawFd as _;

use aterm_spec::derive::{Model, native_update_window_show_model};
use aterm_spec::interp::{self, State};
use aterm_spec::verify::validate_transition_tiered;

use crate::WindowId;
use crate::restore::WindowShow;
use crate::window_show::{CarriedShow, ShowLane, ShowOp, plan};

/// One replay: the App, and what the OS and the user did to it.
struct Rig {
    app: crate::App,
    /// The bootstrap window and the two the rebuild created.
    windows: [WindowId; 3],
    /// The carried key window (window 1, carried in full screen).
    key: WindowId,
    lane: i64,
    /// TRUTH: aterm is the active app.
    front: bool,
    /// The window AppKit keys when aterm is active: the reveal order left the
    /// last window revealed (2) there, and every raise moves it.
    holder: WindowId,
    /// Carried windows typed into while they held the keyboard.
    typed: Vec<WindowId>,
    fs: bool,
    fsfront: bool,
    /// The carry finished (it was recorded and is gone now).
    done: bool,
    /// The update lane's two channels, the ends the outgoing process holds:
    /// the proof is read from the first; the second, never written, is the
    /// Commit this replay delivers through `commit_carried_window_show`
    /// instead (the Commit arm needs a live event loop). Closed on drop, which
    /// ends the real Commit waiter the proof path started with EOF.
    proof_read: Option<std::fs::File>,
    _commit_write: Option<std::fs::File>,
}

/// A fresh pipe: `(read, write)`, one owner each.
fn pipe() -> (std::fs::File, std::fs::File) {
    let mut fds = [0i32; 2];
    // SAFETY: pipe(2) into a valid two-slot out-array.
    assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0, "pipe");
    // SAFETY: two fresh descriptors, owned by exactly one `File` each.
    unsafe {
        (
            std::fs::File::from_raw_fd(fds[0]),
            std::fs::File::from_raw_fd(fds[1]),
        )
    }
}

impl Rig {
    fn new() -> Self {
        let mut app = crate::App::headless_for_test();
        let w0 = WindowId(0);
        let w1 = app.insert_logical_window(crate::stub_session(app.next_session_id), 24, 80);
        let w2 = app.insert_logical_window(crate::stub_session(app.next_session_id), 24, 80);
        // The bootstrap's focus was reported long before the rebuild (the
        // user is in another app); the two new windows start out recorded as
        // focused, unreported; the last is still waiting for its first present.
        app.on_focus(w0, false);
        app.windows.get_mut(&w2).expect("w2").pending_reveal = Some(std::time::Instant::now());
        // Each window's OS identity, which the pre-Commit queue records keys by.
        for (n, window) in [w0, w1, w2].into_iter().enumerate() {
            app.winit_to_window
                .insert(winit::window::WindowId::from(n as u64), window);
        }
        Rig {
            app,
            windows: [w0, w1, w2],
            key: w1,
            lane: 0,
            front: false,
            holder: w2,
            typed: Vec::new(),
            fs: false,
            fsfront: false,
            done: false,
            proof_read: None,
            _commit_write: None,
        }
    }

    /// The update lane's successor OWES the adoption proof: the channels the
    /// outgoing process launched it with, as `main_entry` installs them.
    fn owe_the_proof(&mut self) {
        let (proof_read, proof_write) = pipe();
        let (commit_read, commit_write) = pipe();
        self.app.handoff_ready = Some(crate::seamless::ReadySignal::for_test(
            proof_write.into(),
            "window-show-conformance",
            [0; 32],
            [0; 32],
        ));
        self.app.handoff_commit = Some(crate::seamless::CommitReceiver::for_test(
            commit_read.into(),
        ));
        self.app.handoff_reader_gate = Some(crate::spawn::DeferredReaderGate::closed());
        self.proof_read = Some(proof_read);
        self._commit_write = Some(commit_write);
    }

    /// The carry a rebuild records: window 0 behind window 1, the key window
    /// in full screen, window 2 at the back.
    fn record(&mut self, lane: ShowLane) {
        let [w0, w1, w2] = self.windows;
        let shown = |z_order, key, fullscreen| WindowShow {
            fullscreen: Some(fullscreen),
            minimized: Some(false),
            z_order: Some(z_order),
            key: Some(key),
        };
        let windows = [
            (w0, shown(1, false, false)),
            (w1, shown(0, true, true)),
            (w2, shown(2, false, false)),
        ];
        self.app.carried_window_show = Some(CarriedShow::new(lane, plan(&windows, lane), Some(w0)));
    }

    /// A focus event, the way the `Focused` arm delivers it.
    fn focus(&mut self, window: WindowId, focused: bool) -> Vec<ShowOp<WindowId>> {
        self.app.on_focus(window, focused);
        self.app.carried_window_show_focus_event(window)
    }

    /// AppKit keys `window`: with aterm in front that is a focus change the
    /// App hears about.
    fn key_to(&mut self, window: WindowId) -> Vec<ShowOp<WindowId>> {
        let previous = std::mem::replace(&mut self.holder, window);
        if !self.front || previous == window {
            return Vec::new();
        }
        let mut ops = self.focus(previous, false);
        ops.extend(self.focus(window, true));
        ops
    }

    /// Drive one schedule step on the shipping code; the model actions it
    /// performed, in order.
    fn step(&mut self, action: &'static str) -> Vec<&'static str> {
        let was_stacked = self.stacked();
        let mut performed = Vec::new();
        let mut ops = match action {
            "Launch" | "ColdLaunch" => {
                self.lane = if action == "Launch" { 1 } else { 2 };
                if action == "Launch" {
                    self.owe_the_proof();
                }
                self.record(if action == "Launch" {
                    ShowLane::Handoff
                } else {
                    ShowLane::Cold
                });
                performed.push(action);
                self.app.settle_carried_window_show()
            }
            "Reveal" => {
                let last = self.windows[2];
                self.app.windows.get_mut(&last).expect("w2").pending_reveal = None;
                performed.push(action);
                self.app.settle_carried_window_show()
            }
            "Activate" => {
                self.front = true;
                performed.push(action);
                let holder = self.holder;
                self.focus(holder, true)
            }
            "Deactivate" => {
                self.front = false;
                performed.push(action);
                let holder = self.holder;
                self.focus(holder, false)
            }
            "Steal" => {
                performed.push(action);
                let [w0, _, _] = self.windows;
                let mut ops = self.key_to(w0);
                ops.extend(self.app.settle_carried_window_show());
                ops
            }
            "Type" => {
                performed.push(action);
                self.typed.push(self.holder);
                // Into the real pre-Commit queue, by the holder's OS identity,
                // as the deferral arm queues it — the stack reads it there.
                let winit_id = *self
                    .app
                    .winit_to_window
                    .iter()
                    .find(|(_, window)| **window == self.holder)
                    .expect("the holder's OS identity")
                    .0;
                self.app.queue_pre_commit_input(
                    winit_id,
                    winit::event::WindowEvent::Ime(winit::event::Ime::Commit("ls".into())),
                );
                self.app.settle_carried_window_show()
            }
            "Prove" => {
                performed.push(action);
                // The shipping proof path. It returns nothing, so what its stack
                // performed is read as the plan's `stacking_ops` as it went on —
                // exactly the list `settle_carried_window_show_as` performs
                // (pinned at that seam by `window_show`'s unit tests). Read AFTER
                // the proof: the stack keys a window typed into before it (round
                // six, finding 45), which re-orders the plan it performs.
                self.app.maybe_signal_handoff_ready();
                let stack = self
                    .app
                    .carried_window_show
                    .as_ref()
                    .map(|carried| carried.plan.stacking_ops())
                    .unwrap_or_default();
                assert!(
                    self.app.handoff_ready.is_none(),
                    "the proof path ran to its end"
                );
                let mut proof = [0u8; crate::seamless::READY_WIRE_LEN];
                self.proof_read
                    .as_mut()
                    .expect("the update lane's proof channel")
                    .read_exact(&mut proof)
                    .expect("a whole adoption proof on the wire");
                if !was_stacked && self.stacked() {
                    stack
                } else {
                    Vec::new()
                }
            }
            "Commit" => {
                performed.push(action);
                // The Commit arm's own reading of the queue.
                let typed = self.app.pre_commit_typed_windows();
                assert_eq!(typed, self.typed, "the queue holds what was typed, where");
                self.app.commit_carried_window_show(&typed)
            }
            "Stack" | "EnterFullScreen" => self.app.settle_carried_window_show(),
            other => panic!("no such action {other}"),
        };
        // What the real code did, as the OS sees it: each raise keys its window
        // (and, with aterm in front, reports the change — which can let a
        // waiting full screen go on); each full-screen entry is recorded.
        let mut index = 0;
        while index < ops.len() {
            let op = ops[index];
            match op {
                ShowOp::Raise(window) => {
                    let more = self.key_to(window);
                    ops.extend(more);
                }
                ShowOp::EnterFullScreen(_) => {
                    if !self.fs {
                        self.fs = true;
                        self.fsfront = self.front;
                    }
                }
                ShowOp::Minimize(_) => {}
            }
            index += 1;
        }
        if self.app.carried_window_show.is_none() && self.lane != 0 {
            self.done = true;
        }
        if !was_stacked && self.stacked() {
            performed.push("Stack");
        }
        if ops
            .iter()
            .any(|op| matches!(op, ShowOp::EnterFullScreen(_)))
        {
            performed.push("EnterFullScreen");
        }
        performed
    }

    fn stacked(&self) -> bool {
        self.app
            .carried_window_show
            .as_ref()
            .map_or(self.done, |carried| carried.stacked)
    }

    /// The model's state, read off the App and the replay.
    fn project(&self) -> State {
        let carried = self.app.carried_window_show.as_ref();
        let revealed = self
            .windows
            .iter()
            .all(|window| self.app.windows[window].pending_reveal.is_none());
        let committed = carried.map_or(self.done, |carried| carried.committed);
        let typed_elsewhere = self.typed.iter().any(|window| *window != self.key);
        let mut s: State = BTreeMap::new();
        s.insert("lane", self.lane);
        s.insert("revealed", i64::from(revealed));
        s.insert(
            "proof",
            i64::from(self.lane == 1 && self.app.handoff_ready.is_none()),
        );
        s.insert("stacked", i64::from(self.stacked()));
        s.insert("keyed", i64::from(self.holder == self.key));
        s.insert("typed", i64::from(typed_elsewhere));
        s.insert("committed", i64::from(committed));
        s.insert("front", i64::from(self.front));
        s.insert("fs", i64::from(self.fs));
        s.insert("fsfront", i64::from(self.fsfront));
        s
    }
}

/// The update, the ways it meets the user: in aterm, in another app, with the
/// keyboard moved (and typed into, or not) before Commit, typed into before
/// the stack went on, activated and
/// deactivated in between, and a focus gain between the reveal and the proof
/// (which must not stack); and the cold launch in front and away.
const SCHEDULES: &[&[&str]] = &[
    &["Launch", "Reveal", "Prove", "Activate", "Commit"],
    &["Launch", "Reveal", "Prove", "Commit", "Activate"],
    &[
        "Launch", "Reveal", "Prove", "Steal", "Type", "Commit", "Activate",
    ],
    &[
        "Launch", "Reveal", "Prove", "Activate", "Steal", "Type", "Commit",
    ],
    &["Launch", "Reveal", "Prove", "Steal", "Commit", "Activate"],
    &[
        "Launch",
        "Activate",
        "Deactivate",
        "Reveal",
        "Prove",
        "Commit",
        "Activate",
    ],
    &["Launch", "Reveal", "Activate", "Prove", "Commit"],
    // Typed into BEFORE the proof (round six, finding 45): the window the
    // reveal order keyed keeps the keyboard through the stack and Commit.
    &["Launch", "Reveal", "Type", "Prove", "Commit", "Activate"],
    &["Launch", "Reveal", "Activate", "Type", "Prove", "Commit"],
    &["ColdLaunch", "Activate", "Reveal"],
    &["ColdLaunch", "Reveal", "Activate", "Deactivate"],
];

/// The App actions the real code must take the moment the model allows them.
const EAGER: &[&str] = &["Stack", "EnterFullScreen"];

/// Replay `schedule` against `model`; `(label, conforms)` per step.
fn replay(model: &Model, schedule: &[&'static str], tag: &str) -> Vec<(String, bool)> {
    let mut rig = Rig::new();
    let mut state = rig.project();
    let mut verdicts = vec![(
        format!("{tag} {schedule:?} initial"),
        state == model.init_state(),
    )];
    for &action in schedule {
        let before = state.clone();
        let performed = rig.step(action);
        let after = rig.project();
        let label = format!("{tag} {schedule:?} step {action} performed {performed:?}");
        // Every action but the last is replayed on the model (each must be
        // enabled); the last is checked as a transition onto the real state.
        let mut mid = before.clone();
        let mut conforms = performed.first() == Some(&action);
        if let Some((last, leading)) = performed.split_last() {
            for &earlier in leading {
                conforms &= model.fire(earlier, &mut mid);
            }
            conforms &= validate_transition_tiered(model, &[], &mid, &after, Some(last), &label).0;
        }
        // Eager: nothing the App could still do.
        conforms &= EAGER
            .iter()
            .all(|eager| !model.action_enabled(eager, &after));
        verdicts.push((label, conforms));
        state = after;
    }
    verdicts
}

/// The shipping carry refines `NativeUpdateWindowShow` on every schedule, and
/// every state the schedules reach satisfies every invariant.
#[test]
fn the_real_window_show_carry_conforms_to_the_model() {
    let model = native_update_window_show_model();
    for schedule in SCHEDULES {
        for (label, conforms) in replay(&model, schedule, "committed") {
            assert!(
                conforms,
                "{label} does not conform to NativeUpdateWindowShow"
            );
        }
        let mut rig = Rig::new();
        for &action in *schedule {
            let _ = rig.step(action);
            let state = rig.project();
            for invariant in &model.invariants {
                assert!(
                    model.check_invariant(invariant.name, &state),
                    "{schedule:?} after {action}: {} on {state:?}",
                    invariant.name
                );
            }
        }
        assert!(
            rig.fs,
            "{schedule:?}: every schedule ends back in full screen"
        );
    }
}

/// NEGATIVE CONTROL: `Buggy = 1` is rejected by the real steps — the model
/// that raises before the reveal, or on glass before the proof, finds the real
/// code did not (it waited for the proof path); the one that leaves the reveal
/// order's key finds the real stack keyed the carried window; and the one
/// that re-keys over typing at Commit finds the real Commit left the keyboard
/// where the user typed.
#[test]
fn the_defective_design_rejects_the_real_steps() {
    let buggy = interp::with_buggy(&native_update_window_show_model(), 1);
    let rejected = |verdicts: &[(String, bool)], step: &str| {
        verdicts
            .iter()
            .any(|(label, ok)| label.contains(&format!("step {step} ")) && !ok)
    };
    let early = replay(&buggy, &["Launch", "Reveal", "Prove"], "buggy");
    assert!(rejected(&early, "Launch"), "{early:?}");
    assert!(rejected(&early, "Reveal"), "{early:?}");
    assert!(rejected(&early, "Prove"), "{early:?}");
    let typed = replay(
        &buggy,
        &["Launch", "Reveal", "Prove", "Steal", "Type", "Commit"],
        "buggy-typed",
    );
    assert!(rejected(&typed, "Commit"), "{typed:?}");
}
