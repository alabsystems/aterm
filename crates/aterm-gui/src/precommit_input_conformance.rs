// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance of the successor's pre-Commit input queue to
//! `aterm_models_update::native_update_precommit_input_model`
//! (`NativeUpdatePreCommitInput`, gap #33).
//!
//! Every model schedule up to four steps (enough to reach every model state, which the test
//! checks) is replayed on a headless App with its incoming handoff pending, through the
//! production seams: `queue_pre_commit_input` (input deferred before Commit), `on_focus`
//! (`LoseFocus`) and `settle_replayed_handoff_input` (the Commit arm, run with ⌘ left armed, as a
//! replayed stream whose release was lost would leave it). Every real step must be a transition
//! the model admits (interpreter, and `ty` wherever installed), and every reached state
//! satisfies every invariant.
//!
//! Negative control: `Buggy = 1` (every focus loss marks the queue; an overflow marks nothing)
//! must reject the real steps at the first focus loss with nothing queued and at the overflow.

use std::collections::BTreeMap;

use aterm_models_update::native_update_precommit_input_model;
use aterm_spec::derive::Model;
use aterm_spec::interp::{self, State};
use aterm_spec::verify::validate_transition_tiered;
use aterm_window::event::WindowEvent;
use aterm_window::keyboard::ModifiersState;

use crate::WindowId;
use crate::conformance_support::schedules;

/// What the Commit arm left behind, read at the moment it ran.
#[derive(Clone, Copy)]
struct Settled {
    incoherent: bool,
    dropped: bool,
    repaired: bool,
}

/// One replay: the App, and what the user and the platform did to it.
struct Rig {
    app: crate::App,
    winit: aterm_window::window::WindowId,
    window: WindowId,
    /// Truth: input for the window was queued.
    typed: bool,
    /// Truth: the window lost focus with input already queued for it.
    reordered: bool,
    settled: Option<Settled>,
}

impl Rig {
    fn new() -> Self {
        let mut app = crate::App::headless_for_test();
        let winit = aterm_window::window::WindowId::from(0u64);
        let window = WindowId(0);
        app.winit_to_window.insert(winit, window);
        // The successor, revealed and waiting for Commit.
        app.incoming_handoff_pending = true;
        Rig {
            app,
            winit,
            window,
            typed: false,
            reordered: false,
            settled: None,
        }
    }

    /// The window's input, queued the way `window_event` queues it.
    fn type_one(&mut self) {
        self.app.queue_pre_commit_input(
            self.winit,
            WindowEvent::ModifiersChanged(ModifiersState::SUPER.into()),
        );
        self.typed = true;
    }

    fn step(&mut self, action: &str) {
        match action {
            "Type" => self.type_one(),
            "LoseFocus" => {
                self.reordered |= self.typed;
                self.app.on_focus(self.window, false);
            }
            "Drop" => {
                while self.app.handoff_deferred_input.len() < crate::MAX_DEFERRED_HANDOFF_INPUT {
                    self.type_one();
                }
                self.type_one();
            }
            "Settle" => {
                let incoherent = self.app.handoff_deferred_input_incoherent;
                let dropped = self.app.handoff_deferred_input_dropped > 0;
                // The Commit arm: committed, the queue taken and replayed — and
                // the replay left ⌘ armed, which the settle must disarm exactly
                // when the stream was not coherent.
                self.app.incoming_handoff_pending = false;
                let replayed = std::mem::take(&mut self.app.handoff_deferred_input);
                assert_eq!(
                    !replayed.is_empty(),
                    self.typed,
                    "the queue held what was typed"
                );
                self.app
                    .windows
                    .get_mut(&self.window)
                    .expect("headless window")
                    .mods = ModifiersState::SUPER;
                let _ = self.app.settle_replayed_handoff_input();
                let repaired = self.app.windows[&self.window].mods.is_empty();
                self.settled = Some(Settled {
                    incoherent,
                    dropped,
                    repaired,
                });
            }
            other => panic!("no such action {other}"),
        }
    }

    /// The model's state, read off the App (and, once settled, off what the
    /// Commit arm consumed) and the replay's truth.
    fn project(&self) -> State {
        let queued_for_window = self
            .app
            .handoff_deferred_input
            .iter()
            .any(|(id, _)| *id == self.winit);
        if self.settled.is_none() {
            assert_eq!(
                queued_for_window, self.typed,
                "the real queue holds the window's input"
            );
        }
        let (dropped, incoherent) = self.settled.map_or(
            (
                self.app.handoff_deferred_input_dropped > 0,
                self.app.handoff_deferred_input_incoherent,
            ),
            |settled| (settled.dropped, settled.incoherent),
        );
        let repaired = self.settled.is_some_and(|settled| settled.repaired);
        let stranded = self.settled.is_some() && (self.reordered || dropped) && !repaired;
        let mut s: State = BTreeMap::new();
        s.insert("queued", i64::from(self.typed));
        s.insert("reordered", i64::from(self.reordered));
        s.insert("dropped", i64::from(dropped));
        s.insert("incoherent", i64::from(incoherent));
        s.insert("settled", i64::from(self.settled.is_some()));
        s.insert("repaired", i64::from(repaired));
        s.insert("stranded", i64::from(stranded));
        s
    }
}

const ACTIONS: &[&str] = &["Type", "LoseFocus", "Drop", "Settle"];

/// One real step, projected: `(before, action, after)`.
type Step = (State, &'static str, State);

/// Replay `schedule` on the real App: the projected initial state and every
/// projected step.
fn replay(schedule: &[&'static str]) -> (State, Vec<Step>) {
    let mut rig = Rig::new();
    let initial = rig.project();
    let mut state = initial.clone();
    let mut steps = Vec::new();
    for &action in schedule {
        rig.step(action);
        let after = rig.project();
        steps.push((state, action, after.clone()));
        state = after;
    }
    (initial, steps)
}

/// `(label, conforms)` per step of `schedule` against `model`, both tiers (interpreter, and
/// `ty` wherever installed).
fn verdicts(model: &Model, schedule: &[&'static str], tag: &str) -> Vec<(String, bool)> {
    let (initial, steps) = replay(schedule);
    let mut out = vec![(
        format!("{tag} {schedule:?} initial"),
        initial == model.init_state(),
    )];
    for (before, action, after) in steps {
        let label = format!("{tag} {schedule:?} step {action}");
        let conforms =
            validate_transition_tiered(model, &[], &before, &after, Some(action), &label).0;
        out.push((label, conforms));
    }
    out
}

/// The shipping queue, focus arm and settle refine `NativeUpdatePreCommitInput` on every
/// schedule up to four steps, every state they reach satisfies every invariant, and together
/// they reach every state the model can reach. Each step is admitted by the interpreter; each
/// distinct real transition is then validated once on both tiers (`ty` is a process per call).
#[test]
fn the_real_pre_commit_queue_conforms_to_the_model() {
    // Hold the counters' lock for the whole replay: every `Drop` step refuses a real event
    // (`input_dropped=` + 1) and every incoherent `Settle` repairs one (`input_incoherent=` + 1),
    // process-wide counters the focus and metrics tests pin exactly.
    let _counters = crate::metrics::HANDOFF_INPUT_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let model = native_update_precommit_input_model();
    let all = schedules(&model, ACTIONS, 4);
    assert!(
        all.len() > 40,
        "the enumeration reaches the schedules it exists for: {}",
        all.len()
    );
    let mut distinct: Vec<Step> = Vec::new();
    let mut reached: Vec<State> = vec![model.init_state()];
    for schedule in &all {
        let (initial, steps) = replay(schedule);
        assert_eq!(initial, model.init_state(), "{schedule:?}: initial");
        for (before, action, after) in steps {
            assert!(
                model.successors(action, &before).contains(&after),
                "{schedule:?} step {action}: {before:?} -> {after:?} does not conform to \
                 NativeUpdatePreCommitInput"
            );
            for invariant in &model.invariants {
                assert!(
                    model.check_invariant(invariant.name, &after),
                    "{schedule:?} after {action}: {} on {after:?}",
                    invariant.name
                );
            }
            if !reached.contains(&after) {
                reached.push(after.clone());
            }
            if !distinct.contains(&(before.clone(), action, after.clone())) {
                distinct.push((before, action, after));
            }
        }
    }
    assert_eq!(
        reached.len(),
        interp::bmc(&model).expect("the committed model proves"),
        "the real replays reach every state the model can reach"
    );
    for (before, action, after) in &distinct {
        let label = format!("distinct step {action}: {before:?} -> {after:?}");
        assert!(
            validate_transition_tiered(&model, &[], before, after, Some(action), &label).0,
            "{label} does not conform to NativeUpdatePreCommitInput"
        );
    }
    // The schedules the model exists for are among them.
    for owners in [
        vec!["LoseFocus", "LoseFocus", "Settle"],
        vec!["LoseFocus", "Type", "Settle"],
        vec!["Type", "LoseFocus", "Settle"],
        vec!["Type", "Drop", "Settle"],
    ] {
        assert!(
            all.iter().any(|schedule| schedule.starts_with(&owners)),
            "{owners:?} is replayed"
        );
    }
}

/// Negative control: the latch (`Buggy = 1`: every focus loss marks the queue) rejects the real
/// steps at the first focus loss with nothing queued (winit's startup event for a new window),
/// where the real code leaves the queue coherent; the silent drop (`Buggy = 1`: an overflow
/// marks nothing) rejects them at the overflow, which the real queue marks.
#[test]
fn the_old_latch_rejects_the_real_steps() {
    // Its overflow schedule moves the process-wide counters too.
    let _counters = crate::metrics::HANDOFF_INPUT_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let buggy = interp::with_buggy(&native_update_precommit_input_model(), 1);
    let rejected = |verdicts: &[(String, bool)], step: &str| {
        verdicts
            .iter()
            .any(|(label, ok)| label.ends_with(&format!("step {step}")) && !ok)
    };
    let startup = verdicts(&buggy, &["LoseFocus"], "buggy-startup");
    assert!(rejected(&startup, "LoseFocus"), "{startup:?}");
    let typed_after = verdicts(&buggy, &["LoseFocus", "Type", "Settle"], "buggy-typed");
    assert!(rejected(&typed_after, "LoseFocus"), "{typed_after:?}");
    // …and a loss after typing is where the buggy latch is right: both machines agree there.
    let tabbed = verdicts(&buggy, &["Type", "LoseFocus", "Settle"], "buggy-tabbed");
    assert!(tabbed.iter().all(|(_, ok)| *ok), "{tabbed:?}");
    // The silent drop: the real queue marks the overflow the buggy one ignores.
    let overflowed = verdicts(&buggy, &["Type", "Drop", "Settle"], "buggy-overflow");
    assert!(rejected(&overflowed, "Drop"), "{overflowed:?}");
}
