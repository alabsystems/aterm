// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Conformance tests for layout rebuild sites and adoption proof against `NativeUpdateSuccessorAttach`.
use std::collections::BTreeSet;

use aterm_models_update::native_update_successor_attach_model;
use aterm_spec::derive::Model;
use aterm_spec::interp::{self, State};

use crate::{
    App, RESTORE_GLASS_PATIENCE, RestoreGlassWait, RestorePassSite, WindowId, WindowState,
    handoff_paint_proven, restore_pass_due, restore_pass_sliced,
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
    app.handoff_degraded = s["blocked"] == 1;
    let draining = s["queued"] == 1 && s["draining"] == 1;
    app.pending_restore =
        (s["queued"] == 1 && !draining).then(|| crate::restore::RestoreManifest::new(Vec::new()));
    app.restore_drain = draining.then(|| Box::new(crate::app_restore::RestoreDrain::new(None)));
    let handoff_lane = app.restore_pass_handoff_lane();
    let queued = app.restore_pass_queued();
    let carries_shells = app.restore_pass_carries_shells();
    let first_present_done = s["painted0"] == 1 || s["painted_extras"] == 1;
    let now = std::time::Instant::now();
    app.restore_glass = RestoreGlassWait {
        since: (s["waited"] == 1).then(|| now - RESTORE_GLASS_PATIENCE),
        said: false,
    };
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
        2 if app.restore_pass_due_at_park(first_present_done, now) => {
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

const APP_STEPS: [(&str, &str); 12] = [
    ("RebuildAtAttach", "RebuildAtAttach"),
    ("RebuildWithLostWindow", "RebuildAtAttach"),
    ("RebuildWithUnnoticedLoss", "RebuildAtAttach"),
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
    let mut glassless_rebuilds = 0;
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
        if s["waited"] == 1
            && s["painted0"] + s["painted_extras"] == 0
            && got.contains("RebuildAtPark")
        {
            glassless_rebuilds += 1;
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
    assert!(
        glassless_rebuilds > 0,
        "a park whose first window never reached the glass rebuilds once its patience is spent"
    );
}

/// A cold drain whose first window never reaches the glass waits [`RESTORE_GLASS_PATIENCE`]
/// once, not once per slice: a wait its own slices started again held a twenty-slice restore,
/// and every update behind it, for ten minutes. The model's `waited` stays set through
/// `RebuildSliceAtPark` (its `GlassWaitRearms` mutant is that bug); here the park's own method
/// holds the code to it along one run.
#[test]
fn a_cold_drain_waits_for_its_glass_once_not_once_per_slice() {
    use std::time::Duration;
    let mut app = App::headless_for_test();
    app.restore_drain = Some(Box::new(crate::app_restore::RestoreDrain::new(None)));
    let start = std::time::Instant::now();
    assert!(
        !app.restore_pass_due_at_park(false, start),
        "inside its patience the pass waits for the glass"
    );
    assert_eq!(
        app.restore_glass.deadline(start),
        Some(start + RESTORE_GLASS_PATIENCE),
        "the loop wakes for the wait's end"
    );
    let out = start + RESTORE_GLASS_PATIENCE;
    assert!(
        app.restore_pass_due_at_park(false, out),
        "past it the pass runs without glass"
    );
    for slice in 1..=20 {
        assert!(
            app.restore_pass_due_at_park(false, out + Duration::from_millis(4 * slice)),
            "slice {slice}: the drain under way slices again at the next park"
        );
    }
    assert_eq!(
        app.restore_glass.deadline(out),
        None,
        "no wake for a wait run out"
    );
    assert!(
        !app.restore_glass.say_once(),
        "the glassless run was said once"
    );
    app.restore_drain = None;
    assert!(!app.restore_pass_due_at_park(false, out + Duration::from_secs(1)));
    app.restore_drain = Some(Box::new(crate::app_restore::RestoreDrain::new(None)));
    assert!(
        !app.restore_pass_due_at_park(false, out + Duration::from_secs(2)),
        "a restore queued afresh waits afresh"
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

#[test]
fn a_lost_restore_window_matches_the_model_and_refuses_the_old_mutant() {
    fn next(model: &Model, state: &State, action: &str) -> State {
        let successors = model.successors(action, state);
        assert_eq!(successors.len(), 1, "{action} from {state:?}");
        successors[0].clone()
    }
    fn before_rebuild(model: &Model) -> State {
        let mut state = model.init_state();
        for action in ["Widen", "StartHandoff", "Attach0", "Present0"] {
            state = next(model, &state, action);
        }
        state
    }

    let model = interp::with_buggy(&native_update_successor_attach_model(), 0);
    let lost = next(&model, &before_rebuild(&model), "RebuildWithLostWindow");
    let (mut real, _keep) = Real::new();
    let failed = real.handoff.insert_logical_window(
        crate::stub_session(real.handoff.next_session_id),
        24,
        80,
    );
    real.handoff.retire_unattached_restored_window(failed);
    assert_eq!(
        lost["extras"],
        i64::from(real.handoff.windows.contains_key(&failed))
    );
    assert_eq!(
        lost["blocked"],
        i64::from(!real.handoff.restore_pass_handoff_lane())
    );
    let shipping = real_steps(&mut real, &lost);
    assert_eq!(shipping, model_steps(&model, &lost));
    assert!(!shipping.contains("Prove"));

    let buggy = interp::with_buggy(&native_update_successor_attach_model(), 1);
    let unnoticed = next(&buggy, &before_rebuild(&buggy), "RebuildWithUnnoticedLoss");
    assert!(buggy.action_enabled("Prove", &unnoticed));
    let proved = next(&buggy, &unnoticed, "Prove");
    assert!(!buggy.check_invariant("ProofRetainsCompleteLayout", &proved));
    assert_ne!(unnoticed["blocked"], lost["blocked"]);
}
