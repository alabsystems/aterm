// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Conformance tests for layout rebuild sites and adoption proof against `NativeUpdateSuccessorAttach`.
use std::collections::BTreeSet;

use aterm_models_update::native_update_successor_attach_model;
use aterm_spec::derive::Model;
use aterm_spec::interp::{self, State};

use crate::{
    App, RestorePassSite, WindowId, WindowState, handoff_paint_proven, restore_pass_due,
    restore_pass_sliced,
};

struct Real {
    handoff: App,
    degraded: App,
    cold: App,
    /// A window that has not presented, and one that has.
    windows: [Box<WindowState>; 2],
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

    fn app(&mut self, lane: i64, handed: bool) -> &mut App {
        match (lane, handed) {
            (1, _) => &mut self.handoff,
            (_, true) => &mut self.degraded,
            _ => &mut self.cold,
        }
    }
}

fn window(painted: bool) -> Box<WindowState> {
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

fn real_steps(real: &mut Real, s: &State) -> BTreeSet<&'static str> {
    let lane = s["lane"];
    let mut steps = BTreeSet::new();
    if lane == 0 {
        return steps;
    }
    let app = real.app(lane, s["handed"] == 1);
    let draining = s["queued"] == 1 && s["draining"] == 1;
    app.pending_restore =
        (s["queued"] == 1 && !draining).then(|| crate::restore::RestoreManifest::new(Vec::new()));
    app.restore_drain = draining.then(|| Box::new(crate::app_restore::RestoreDrain::new(None)));
    let handoff_lane = app.restore_pass_handoff_lane();
    let queued = app.restore_pass_queued();
    let carries_shells = app.restore_pass_carries_shells();
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
            if restore_pass_sliced(RestorePassSite::FirstAttach, carries_shells) {
                steps.insert("SliceAtPark");
            }
        }
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
            if restore_pass_sliced(RestorePassSite::Park, carries_shells) {
                steps.insert("SliceAtPark");
            }
        }
        _ => {}
    }
    let drained = !queued;
    let [unpainted, painted_window] = &real.windows;
    let (unpainted, painted_window): (&WindowState, &WindowState) = (unpainted, painted_window);
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

const APP_STEPS: [(&str, &str); 10] = [
    ("RebuildAtAttach", "RebuildAtAttach"),
    ("LeaveResume", "LeaveResume"),
    ("RebuildAtPark", "RebuildAtPark"),
    ("RebuildSliceAtPark", "SliceAtPark"),
    ("Prove", "Prove"),
    ("RebuildAtAttachCold", "RebuildAtAttach"),
    ("LeaveResumeOwing", "LeaveResume"),
    ("ProveAtFirstPresent", "Prove"),
    ("RebuildSliceHanded", "SliceAtPark"),
    ("RebuildSliceBeforePaint", "SliceAtPark"),
];

fn model_steps(model: &Model, s: &State) -> BTreeSet<&'static str> {
    APP_STEPS
        .into_iter()
        .filter(|(action, _)| model.action_enabled(action, s))
        .map(|(_, step)| step)
        .collect()
}

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
    let mut cold_slices = 0;
    for s in &states {
        let want = model_steps(&model, s);
        let got = real_steps(&mut real, s);
        assert_eq!(got, want, "state {s:?}");
        if model.action_enabled("RebuildAtAttach", s) && s["painted0"] == 0 && s["many"] == 1 {
            attach_passes += 1;
        }
        if s["draining"] == 1 && s["queued"] == 1 && got.contains("SliceAtPark") {
            cold_slices += 1;
        }
    }
    assert!(
        attach_passes > 0,
        "the parallel attach is reached: extra windows built before window 0 has presented"
    );
    assert!(
        cold_slices > 0,
        "a drain under way is reached, and its next park slices again"
    );
}

#[test]
fn negative_control_the_buggy_family_disagrees_with_the_shipping_code() {
    let buggy = interp::with_buggy(&native_update_successor_attach_model(), 1);
    let (mut real, _keep) = Real::new();
    let mut disagreements = BTreeSet::new();
    for s in reachable(&buggy) {
        let want = model_steps(&buggy, &s);
        let got = real_steps(&mut real, &s);
        for step in want.symmetric_difference(&got) {
            disagreements.insert(*step);
        }
    }
    for step in ["RebuildAtAttach", "LeaveResume", "SliceAtPark", "Prove"] {
        assert!(
            disagreements.contains(step),
            "the shipping code must refuse the mutant's {step}: {disagreements:?}"
        );
    }
}
