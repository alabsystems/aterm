// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance of where the layout rebuild runs and what the adoption
//! proof waits for (warm successor P3, the parallel attach) to
//! `aterm_spec::derive::native_update_successor_attach_model`
//! (`NativeUpdateSuccessorAttach`).
//!
//! Every reachable state of the committed model is projected onto the
//! SHIPPING decisions: a headless App's own lane and queue reads
//! (`App::restore_pass_handoff_lane`, `App::restore_pass_queued`), the site
//! decision both call sites make (`restore_pass_due`, `FirstAttach` in
//! `resumed` and `Park` in `about_to_wait`), and the proof's paint decision
//! over real `WindowState`s (`handoff_paint_proven`, fed the drained queues as
//! `App::maybe_signal_handoff_ready` feeds it). Each of the App's guarded
//! steps — `RebuildAtAttach`, `LeaveResume`, `RebuildAtPark`, `Prove` — must be
//! enabled in the model exactly when the real code takes it.
//!
//! NEGATIVE CONTROL: the same real decisions against `Buggy = 1` (the attach
//! pass on the cold lane, the handoff lane waiting for the park, a proof at
//! window 0's first present) must disagree somewhere, so a pass is never
//! vacuous.

use std::collections::BTreeSet;

use aterm_spec::derive::{Model, native_update_successor_attach_model};
use aterm_spec::interp::{self, State};

use crate::{App, RestorePassSite, WindowId, WindowState, handoff_paint_proven, restore_pass_due};

/// The four guarded App steps, as the shipping code decides each in `state`.
struct Real {
    handoff: App,
    degraded: App,
    cold: App,
    /// A window that has not presented, and one that has.
    windows: [WindowState; 2],
}

impl Real {
    fn new() -> (Self, [std::os::fd::OwnedFd; 2]) {
        use std::os::fd::FromRawFd as _;
        let mut ready = [0i32; 2];
        let mut spare = [0i32; 2];
        // SAFETY: both arrays provide pipe(2)'s two writable descriptor slots.
        assert_eq!(unsafe { libc::pipe(ready.as_mut_ptr()) }, 0, "ready pipe");
        assert_eq!(unsafe { libc::pipe(spare.as_mut_ptr()) }, 0, "spare pipe");
        // SAFETY: four fresh descriptors, one owner each from here.
        let (ready_read, ready_write, spare_read, spare_write) = unsafe {
            (
                std::os::fd::OwnedFd::from_raw_fd(ready[0]),
                std::os::fd::OwnedFd::from_raw_fd(ready[1]),
                std::os::fd::OwnedFd::from_raw_fd(spare[0]),
                std::os::fd::OwnedFd::from_raw_fd(spare[1]),
            )
        };
        let mut handoff = App::headless_for_test();
        handoff.handoff_ready = Some(crate::seamless::ReadySignal::for_test(
            ready_write,
            "restore-pass",
            [1; 32],
            [2; 32],
        ));
        // A degraded handoff never proves: it keeps the cold lane's order.
        let mut degraded = App::headless_for_test();
        degraded.handoff_ready = Some(crate::seamless::ReadySignal::for_test(
            spare_write,
            "restore-pass-degraded",
            [3; 32],
            [4; 32],
        ));
        degraded.handoff_degraded = true;
        let cold = App::headless_for_test();
        (
            Self {
                handoff,
                degraded,
                cold,
                windows: [window(false), window(true)],
            },
            [ready_read, spare_read],
        )
    }

    /// The App whose lane the model's `lane` names; `alt` picks the degraded
    /// handoff for the cold lane's second face.
    fn app(&mut self, lane: i64, alt: bool) -> &mut App {
        match (lane, alt) {
            (1, _) => &mut self.handoff,
            (_, true) => &mut self.degraded,
            _ => &mut self.cold,
        }
    }
}

fn window(painted: bool) -> WindowState {
    let mut ws = App::headless_for_test()
        .windows
        .remove(&WindowId(0))
        .expect("the test App's window");
    assert!(ws.pending_reveal.is_none());
    if painted {
        ws.on_present_succeeded();
    }
    ws
}

/// Which of the App's guarded steps the SHIPPING code takes in `s`.
fn real_steps(real: &mut Real, s: &State, alt: bool) -> BTreeSet<&'static str> {
    let lane = s["lane"];
    let mut steps = BTreeSet::new();
    if lane == 0 {
        return steps;
    }
    let app = real.app(lane, alt);
    app.pending_restore =
        (s["queued"] == 1).then(|| crate::restore::RestoreManifest::new(Vec::new()));
    let handoff_lane = app.restore_pass_handoff_lane();
    let queued = app.restore_pass_queued();
    let first_present_done = s["painted0"] == 1 || s["painted_extras"] == 1;
    let at_attach = restore_pass_due(
        RestorePassSite::FirstAttach,
        handoff_lane,
        first_present_done,
        queued,
    );
    match s["phase"] {
        1 if at_attach => {
            steps.insert("RebuildAtAttach");
        }
        // `resumed` returns once its pass ran or was not due.
        1 => {
            steps.insert("LeaveResume");
        }
        2 if restore_pass_due(
            RestorePassSite::Park,
            handoff_lane,
            first_present_done,
            queued,
        ) =>
        {
            steps.insert("RebuildAtPark");
        }
        _ => {}
    }
    // The proof: drained queues and the paint decision over the attached
    // windows, as `maybe_signal_handoff_ready` reads them.
    let drained = !queued;
    let [unpainted, painted_window] = &real.windows;
    let pick = |painted: i64| {
        if painted == 1 {
            painted_window
        } else {
            unpainted
        }
    };
    let mut attached = vec![pick(s["painted0"])];
    if s["extras"] == 1 {
        attached.push(pick(s["painted_extras"]));
    }
    let painted = handoff_paint_proven(false, first_present_done, attached.into_iter());
    if handoff_lane && s["proved"] == 0 && drained && painted {
        steps.insert("Prove");
    }
    steps
}

/// The model's actions for the App's guarded steps, each with the step it is
/// (a `Buggy = 1` mutant takes the step it corrupts). The mutant-only
/// liveness action, `RebuildWithoutKick`, is no guard decision: it builds
/// windows without their first present, which this bind does not drive.
const APP_STEPS: [(&str, &str); 7] = [
    ("RebuildAtAttach", "RebuildAtAttach"),
    ("LeaveResume", "LeaveResume"),
    ("RebuildAtPark", "RebuildAtPark"),
    ("Prove", "Prove"),
    ("RebuildAtAttachCold", "RebuildAtAttach"),
    ("LeaveResumeOwing", "LeaveResume"),
    ("ProveAtFirstPresent", "Prove"),
];

fn model_steps(model: &Model, s: &State) -> BTreeSet<&'static str> {
    APP_STEPS
        .into_iter()
        .filter(|(action, _)| model.action_enabled(action, s))
        .map(|(_, step)| step)
        .collect()
}

/// Every state `model` reaches, `RebuildWithoutKick` left out (see
/// [`APP_STEPS`]).
fn reachable(model: &Model) -> Vec<State> {
    let mut seen = BTreeSet::new();
    let mut todo = vec![model.init_state()];
    let mut out = Vec::new();
    while let Some(state) = todo.pop() {
        if !seen.insert(state.clone()) {
            continue;
        }
        for action in model.actions.iter().map(|a| a.name) {
            if action == "RebuildWithoutKick" {
                continue;
            }
            todo.extend(model.successors(action, &state));
        }
        out.push(state);
    }
    out
}

#[test]
fn the_shipping_rebuild_sites_and_proof_gate_conform_to_the_model() {
    let model = interp::with_buggy(&native_update_successor_attach_model(), 0);
    let (mut real, _keep) = Real::new();
    let states = reachable(&model);
    assert!(
        states.len() > 30,
        "the walk covers the machine: {}",
        states.len()
    );
    let mut attach_passes = 0;
    for s in &states {
        for alt in [false, true] {
            let want = model_steps(&model, s);
            let got = real_steps(&mut real, s, alt);
            assert_eq!(got, want, "state {s:?} (degraded face: {alt})");
        }
        if model.action_enabled("RebuildAtAttach", s) && s["painted0"] == 0 && s["many"] == 1 {
            attach_passes += 1;
        }
    }
    assert!(
        attach_passes > 0,
        "the parallel attach is reached: extra windows built before window 0 has presented"
    );
}

#[test]
fn negative_control_the_buggy_family_disagrees_with_the_shipping_code() {
    let buggy = interp::with_buggy(&native_update_successor_attach_model(), 1);
    let (mut real, _keep) = Real::new();
    let mut disagreements = BTreeSet::new();
    for s in reachable(&buggy) {
        let want = model_steps(&buggy, &s);
        let got = real_steps(&mut real, &s, false);
        for step in want.symmetric_difference(&got) {
            disagreements.insert(*step);
        }
    }
    for step in ["RebuildAtAttach", "LeaveResume", "Prove"] {
        assert!(
            disagreements.contains(step),
            "the shipping code must refuse the mutant's {step}: {disagreements:?}"
        );
    }
}
