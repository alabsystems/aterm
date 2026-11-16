// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 for `ExplainHonesty`: native published counters carry no execution-window assurance.

use std::collections::BTreeMap;

use aterm_models_misc::explain_honesty_model;
use aterm_spec::derive::Model;
use aterm_spec::interp;
use aterm_vitals::explain::fixtures::{
    Scene, calm_1510, idle_skewed_1510, spotlight_1032, tab2_build_1048,
};
use aterm_vitals::explain::{
    Bare, CauseId, Context, Role, WhySubject, account, bare_subject, deck_cause, you_role,
};
use aterm_vitals::model::{Pressure, Thermal, Throttle};
use aterm_vitals::published_cpu::{CpuBasis, CpuExecutionWindow};
use aterm_vitals::{Snapshot, Vitals};

type State = BTreeMap<&'static str, i64>;

fn read(scene: &Scene) -> Snapshot {
    let mut vitals = Vitals::new();
    let [before, sweep, after] = scene.samples();
    vitals.absorb(&before);
    vitals.absorb(&sweep);
    vitals.absorb(&after)
}

fn complete(mut scene: Scene) -> Scene {
    scene.unanswered = 0;
    for coalition in &mut scene.coalitions {
        coalition.started = coalition.exited
            + scene
                .procs
                .iter()
                .filter(|process| process.coalition == coalition.id)
                .count() as u64;
    }
    scene
}

fn cases() -> Vec<(Snapshot, u64)> {
    let mut pressured = spotlight_1032();
    pressured.mem = calm_1510().mem;
    pressured.mem.as_mut().unwrap().pressure = Pressure::Warn;
    pressured.thermal = Some(Thermal {
        level: Throttle::Light,
        ..Thermal::default()
    });
    vec![
        (read(&spotlight_1032()), 1),
        (read(&tab2_build_1048()), 2),
        (read(&complete(spotlight_1032())), 1),
        (read(&complete(idle_skewed_1510())), 1),
        (read(&pressured), 1),
    ]
}

/// The source cannot reveal hidden execution. The same observations permit one unit of
/// execution inside the interval and four unpublished units from before it. `reported` is
/// abstracted to zero/positive, not asserted to equal either execution quantity.
fn project(model: &Model, snapshot: &Snapshot, tab: u64) -> [State; 2] {
    let facts = snapshot
        .why
        .as_ref()
        .expect("fixture produced a counter observation");
    let cpu = account(WhySubject::Cpu, Some(tab), snapshot);
    let reported = facts.split.tab_reported_of(&[tab]);
    assert_eq!(
        cpu.you.figure,
        Some(reported),
        "preserve the actual diagnostic value"
    );
    assert_eq!(
        facts.split.cpu_accounting.basis,
        CpuBasis::PublishedCounters
    );
    let window = match facts.split.cpu_accounting.execution_window {
        CpuExecutionWindow::Unavailable => 0,
    };
    let independent_context = snapshot
        .thermal
        .is_some_and(|thermal| thermal.throttled() || thermal.low_power == Some(true))
        || snapshot
            .mem
            .as_ref()
            .is_some_and(|memory| memory.sample.pressure.rank().is_some_and(|rank| rank >= 1));
    let mut before = model.init_state();
    before.extend([
        ("prior", 4),
        ("actual", 1),
        ("reported", i64::from(reported > 0)),
        ("complete", i64::from(facts.split.coverage_complete)),
        ("context", i64::from(independent_context)),
    ]);
    let mut after = before.clone();
    after.extend([
        ("fresh", 1),
        ("window", window),
        (
            "role",
            i64::from(!matches!(cpu.you.role, Role::Unavailable(_))),
        ),
        ("share", i64::from(cpu.you.share_pm.is_some())),
        (
            "cause",
            i64::from(
                cpu.causes.iter().any(|shown| shown.cause.id.covering())
                    || facts.causes.iter().any(|cause| cause.id.covering())
                    || facts.open.iter().any(|cause| cause.id.covering()),
            ),
        ),
        ("residual", i64::from(cpu.you.unattributed_mc.is_some())),
        (
            "none",
            i64::from(cpu.nothing_needed || cpu.you.calm || bare_subject(snapshot) == Bare::Calm),
        ),
        ("clause", i64::from(deck_cause(snapshot).is_some())),
        (
            "diagnostic",
            i64::from(cpu.you.figure.is_some_and(|figure| figure > 0)),
        ),
        (
            "shown_context",
            i64::from(cpu.causes.iter().any(|shown| {
                matches!(
                    shown.cause.id,
                    CauseId::Context(Context::Pressure | Context::Throttled)
                )
            })),
        ),
    ]);
    [before, after]
}

fn broken(model: &Model, states: &[State; 2]) -> Vec<&'static str> {
    model
        .invariants
        .iter()
        .filter(|law| {
            states
                .iter()
                .any(|state| !model.check_invariant(law.name, state))
        })
        .map(|law| law.name)
        .collect()
}

#[test]
fn reported_cpu_production_observations_conform_without_physical_assurance() {
    let model = explain_honesty_model();
    let mut saw_complete = false;
    let mut saw_incomplete = false;
    let mut saw_zero = false;
    let mut saw_context = false;
    for (snapshot, tab) in cases() {
        let states = project(&model, &snapshot, tab);
        assert_eq!(
            interp::admits(&model, &states[0], &states[1]),
            Some("Evaluate")
        );
        assert!(broken(&model, &states).is_empty());
        saw_complete |= states[0]["complete"] == 1;
        saw_incomplete |= states[0]["complete"] == 0;
        saw_zero |= states[0]["reported"] == 0;
        saw_context |= states[0]["context"] == 1;
    }
    assert!(saw_complete && saw_incomplete && saw_zero && saw_context);
}

#[test]
fn reported_cpu_legacy_role_promotion_is_rejected_and_has_a_model_counterexample() {
    let model = explain_honesty_model();
    let snapshot = read(&complete(idle_skewed_1510()));
    let mut states = project(&model, &snapshot, 1);
    assert_eq!((states[0]["actual"], states[0]["reported"]), (1, 0));
    let cpu = account(WhySubject::Cpu, Some(1), &snapshot);
    assert_eq!(cpu.you.role, Role::Unavailable("execution_window_unknown"));
    // Negative control: the old promotion treats a zero published figure as Role::None.
    // This uses the real memory-prominence helper on CPU input, recreating the category error.
    let legacy = you_role(
        cpu.you.figure.unwrap(),
        snapshot.why.as_ref().unwrap().split.global_mc,
        true,
        true,
    )
    .0;
    assert_eq!(legacy, Role::None);
    states[1].insert("role", i64::from(!matches!(legacy, Role::Unavailable(_))));
    assert_eq!(interp::admits(&model, &states[0], &states[1]), None);
    let mutant = interp::with_buggy(&model, 1);
    assert_eq!(
        interp::admits(&mutant, &states[0], &states[1]),
        Some("Evaluate")
    );
    assert_eq!(broken(&mutant, &states), ["NoRoleWithoutExecutionWindow"]);
}
