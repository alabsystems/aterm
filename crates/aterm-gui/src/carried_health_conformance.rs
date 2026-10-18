// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 for `NativeUpdateCarriedHealth` (aterm-spec's `native_update_carried_health_model`):
//! a check warning and a stall warning a parent raised cross a self-update into a real
//! successor `App`.
//!
//! Every transition of the committed model (`Buggy = 0`) is replayed against the shipping seed
//! ([`App::seed_carried_messages`]), the one door ([`App::note_update_health`]) and the
//! watchdog's look ([`App::look_at_update_checker`]); after each step the successor is
//! projected onto the model's variables ([`project`]). The updater stands in as one flag,
//! `owes`, set from what the seed handed it (`UPDATER_ADOPTED_HEALTH`); its own transitions are
//! aterm-update's `persistent_notice_tests`. The ledger and this process's completed checks are
//! the environment.
//!
//! Negative control: each `Buggy = 1` mutant's counterexample, replayed on the real successor,
//! breaks the invariant the model says it breaks, while the shipping step at the same point
//! keeps it.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::time::Instant;

use aterm_models_update::native_update_carried_health_model;
use aterm_spec::derive::Model;
use aterm_spec::interp::{self, State};
use aterm_spec::xref::StepEvidence;

use super::tests::{owes_entry, window_carry};
use super::{HealthProof, UPDATER_ADOPTED_HEALTH};
use crate::App;
use crate::update_words::{self, HealthKind};

/// The model's variables read from the successor. `phase` and `ledger` are
/// the driver's position and the environment, carried from the model state.
const PROJECTED: [&str; 7] = ["owed", "up", "entry", "checked", "armed", "stall", "sentry"];

/// The actions a step window drives, each an anchored shipping call.
const ANCHORED: [&str; 4] = ["Seed", "Recover", "Announce", "StallHeals"];

/// The `Buggy = 1` actions, dead in the committed model.
const MUTANTS: [&str; 5] = [
    "SeedRemembersOnly",
    "SeedStallRemembersOnly",
    "RecoverHealsStall",
    "SeedForgetsEntry",
    "StallHealKeepsEntry",
];

const BODY: &str = "20 failed checks in a row since 2026-09-14T22:04:36Z: the reason.";
const LATER: &str = "3 failed checks in a row since 2026-09-29T15:00:00Z: another reason.";

/// A parent's carry and the successor it seeds.
struct Rig {
    carry: crate::session_store::WindowCarry,
    successor: App,
    /// The stand-in updater's announced word holds the check class.
    owes: bool,
    /// This process's checker's completed checks.
    checks: u64,
}

impl Rig {
    fn new() -> Self {
        Self {
            carry: parent_carry(aterm_update::health_failing_title("manifest")),
            successor: App::headless_for_test(),
            owes: false,
            checks: 0,
        }
    }

    /// This process's checker as the watchdog reads it.
    fn beat(&self) -> aterm_update::checker_watch::CheckerBeat {
        aterm_update::checker_watch::CheckerBeat {
            generation: 1,
            phase: aterm_update::checker_watch::CheckerPhase::Waiting,
            at_secs: 100,
            max_interval_secs: 1_100,
            respawns: 0,
            deferrals: 0,
            settings_misses: 0,
            lane: None,
            checks: self.checks,
        }
    }

    /// After the seed: Commit and the landing, as the successor meets them.
    fn land(&mut self) {
        self.successor.messages.after_handoff_commit(Instant::now());
        self.successor.post_update_landed("9.9.9", 7, 0);
        self.owes = UPDATER_ADOPTED_HEALTH
            .with(|adopted| std::mem::take(&mut *adopted.borrow_mut()))
            .contains(&"manifest");
    }
}

/// The carry of a parent that said a check warning under `title` and a stall.
fn parent_carry(title: &str) -> crate::session_store::WindowCarry {
    let mut parent = App::headless_for_test();
    assert!(parent.note_update_health(title, BODY));
    let stalled = update_words::checker_stalled_body(3_000, "checking", true, false);
    assert!(parent.note_update_health_as(
        HealthKind::Stalled,
        update_words::CHECKER_STALLED_TITLE,
        &stalled
    ));
    window_carry(parent.message_band_rows, parent.carried_messages())
}

/// `model_state` with every [`PROJECTED`] variable read from the successor.
fn project(rig: &Rig, model_state: &State) -> State {
    let app = &rig.successor;
    let mut state = model_state.clone();
    state.insert("owed", i64::from(rig.owes));
    let stands = |kind: HealthKind| app.update_health_said.iter().any(|said| said.kind == kind);
    state.insert("up", i64::from(stands(HealthKind::Check)));
    state.insert("entry", i64::from(owes_entry(app, HealthKind::Check)));
    state.insert("checked", i64::from(rig.checks > 0));
    state.insert(
        "armed",
        i64::from(app.update_checker_watch.owes_stall_heal()),
    );
    state.insert("stall", i64::from(stands(HealthKind::Stalled)));
    state.insert("sentry", i64::from(owes_entry(app, HealthKind::Stalled)));
    state
}

/// Drive `action` on the rig: the anchored calls inside their own window, the
/// environment's outside any.
fn drive(rig: &mut Rig, action: &'static str, ev: &mut StepEvidence) {
    match action {
        "Seed" => {
            UPDATER_ADOPTED_HEALTH.with(|adopted| adopted.borrow_mut().clear());
            ev.step(action, || {
                rig.successor
                    .seed_carried_messages(&rig.carry, Some("9.9.9"));
            });
            rig.land();
        }
        "Recover" => {
            let said = ev.step(action, || {
                rig.successor
                    .note_update_health(aterm_update::HEALTH_RECOVERED_TITLE, "manifest")
            });
            assert!(!said);
            rig.owes = false;
        }
        "Announce" => {
            let said = ev.step(action, || {
                rig.successor
                    .note_update_health(aterm_update::health_failing_title("manifest"), LATER)
            });
            assert!(said, "a new episode with nothing standing is said");
            rig.owes = true;
        }
        "StallHeals" => {
            let beat = rig.beat();
            ev.step(action, || {
                rig.successor.look_at_update_checker(Some(beat), 200);
            });
        }
        "CheckCompletes" => rig.checks += 1,
        "LedgerHeals" | "LedgerFails" => {}
        other => panic!("no real step for `{other}`"),
    }
}

/// Replay `schedule` on a fresh rig, checking the projection after every step.
/// `stand_in` realizes a `Buggy = 1` mutant's step; every other action is
/// [`drive`]n.
fn replay(
    model: &Model,
    schedule: &[&'static str],
    ev: &mut StepEvidence,
    stand_in: &dyn Fn(&mut Rig, &str) -> bool,
) -> (Rig, State) {
    let mut rig = Rig::new();
    let mut state = model.init_state();
    let mut real = project(&rig, &state);
    assert_eq!(real, state, "the fresh successor is the model's start");
    for &action in schedule {
        assert!(
            model.fire(action, &mut state),
            "{action} is enabled after {schedule:?}: {state:?}"
        );
        if !stand_in(&mut rig, action) {
            drive(&mut rig, action, ev);
        }
        real = project(&rig, &state);
        for var in PROJECTED {
            assert_eq!(
                real[var], state[var],
                "`{var}` after {action} in {schedule:?}: real {real:?}, model {state:?}"
            );
        }
    }
    (rig, real)
}

/// One schedule per reachable transition of `model`: the shortest path to its
/// source state, then the transition.
fn transitions(model: &Model) -> Vec<Vec<&'static str>> {
    let mut path: BTreeMap<State, Vec<&'static str>> = BTreeMap::new();
    let mut queue = VecDeque::new();
    path.insert(model.init_state(), Vec::new());
    queue.push_back(model.init_state());
    let mut out = Vec::new();
    while let Some(state) = queue.pop_front() {
        for action in model.actions.iter().map(|a| a.name) {
            for next in model.successors(action, &state) {
                let mut schedule = path[&state].clone();
                schedule.push(action);
                out.push(schedule.clone());
                if !path.contains_key(&next) {
                    path.insert(next.clone(), schedule);
                    queue.push_back(next);
                }
            }
        }
    }
    out
}

#[test]
fn the_shipping_seed_door_and_watchdog_conform_to_the_model() {
    let model = interp::with_buggy(&native_update_carried_health_model(), 0);
    let mut ev = StepEvidence::new("native_update_carried_health");
    let schedules = transitions(&model);
    assert!(
        schedules.len() > 20,
        "the walk covers the machine: {} transitions",
        schedules.len()
    );
    let mut fired: BTreeSet<&str> = BTreeSet::new();
    for schedule in &schedules {
        let (_rig, real) = replay(&model, schedule, &mut ev, &|_, _| false);
        for invariant in &model.invariants {
            assert!(
                model.check_invariant(invariant.name, &real),
                "{} after {schedule:?}",
                invariant.name
            );
        }
        fired.extend(schedule.iter().copied());
    }
    let committed: BTreeSet<&str> = model
        .actions
        .iter()
        .map(|a| a.name)
        .filter(|name| !MUTANTS.contains(name))
        .collect();
    assert_eq!(fired, committed, "every committed action is driven");

    let audit = ev.audit();
    let actions: BTreeSet<&str> = audit.actions.iter().copied().collect();
    assert_eq!(actions, ANCHORED.into_iter().collect::<BTreeSet<_>>());
    assert!(audit.uninstrumented.is_empty(), "{audit:?}");
    assert!(audit.ambiguous.is_empty(), "{audit:?}");
    assert!(audit.unanchored_steps.is_empty(), "{audit:?}");
    assert!(audit.undriven.is_empty(), "{audit:?}");
    assert!(audit.unwitnessed.is_empty(), "{audit:?}");
    assert!(audit.blind_steps.is_empty(), "{audit:?}");
    assert!(audit.stray.is_empty(), "{audit:?}");
}

/// The stand-ins for the `Buggy = 1` mutants, on the real successor. `true`
/// when the action was realized here.
fn mutant_stand_in(rig: &mut Rig, action: &str) -> bool {
    let seed = |rig: &mut Rig| {
        UPDATER_ADOPTED_HEALTH.with(|adopted| adopted.borrow_mut().clear());
        rig.successor
            .seed_carried_messages(&rig.carry, Some("9.9.9"));
        rig.land();
    };
    match action {
        // Keep the warning without handing its class to the updater: what the shipping seed
        // does with a check warning whose title names no ledger class (the strand notice).
        "SeedRemembersOnly" => {
            rig.carry = parent_carry(aterm_update::STRANDED_TITLE);
            seed(rig);
            true
        }
        // Keep the stall without arming the watchdog's heal.
        "SeedStallRemembersOnly" => {
            seed(rig);
            rig.successor.update_checker_watch = Default::default();
            true
        }
        // The parent's pending claim of the check's line dies with it.
        "SeedForgetsEntry" => {
            seed(rig);
            rig.successor
                .except_clear(&update_words::update_health_entry(HealthKind::Check, "").key());
            true
        }
        // The ledger's healing read as `Checked`, which answers the stall.
        "RecoverHealsStall" => {
            rig.successor
                .heal_update_health(HealthProof::healed_by_ledger("manifest"));
            rig.owes = false;
            true
        }
        // The stall's heal with its digest claim left ripening: latch, rows and record only.
        "StallHealKeepsEntry" => {
            let stalled = HealthKind::Stalled;
            let app = &mut rig.successor;
            app.update_checker_watch = Default::default();
            app.update_health_latched.retain(|kind| *kind != stalled);
            for id in app.live_update_health(|kind| kind == stalled) {
                app.resolve_message(id, aterm_messages::Outcome::Warn);
            }
            app.record_update_health_healed(|kind| kind == stalled);
            true
        }
        _ => false,
    }
}

/// Each mutant breaks its invariant on the real successor; the shipping step at the same point
/// keeps it.
#[test]
fn negative_control_each_mutant_breaks_its_invariant_on_the_real_successor() {
    let buggy = interp::with_buggy(&native_update_carried_health_model(), 1);
    let committed = interp::with_buggy(&native_update_carried_health_model(), 0);
    let mut ev = StepEvidence::new("native_update_carried_health");
    let cases: [(&[&'static str], &str, &[&'static str]); 5] = [
        (&["SeedRemembersOnly"], "WarningHasHealer", &["Seed"]),
        (&["SeedStallRemembersOnly"], "StallHasHealer", &["Seed"]),
        (
            &["Seed", "LedgerHeals", "RecoverHealsStall"],
            "StallStandsUntilACheckHere",
            &["Seed", "LedgerHeals", "Recover"],
        ),
        (&["SeedForgetsEntry"], "CheckLineFollowsWarning", &["Seed"]),
        (
            &["Seed", "CheckCompletes", "StallHealKeepsEntry"],
            "StallLineFollowsWarning",
            &["Seed", "CheckCompletes", "StallHeals"],
        ),
    ];
    for (mutant, invariant, shipping) in cases {
        let (_rig, real) = replay(&buggy, mutant, &mut ev, &mutant_stand_in);
        assert!(
            !buggy.check_invariant(invariant, &real),
            "{mutant:?} breaks {invariant} on the real successor: {real:?}"
        );
        let (_rig, real) = replay(&committed, shipping, &mut ev, &|_, _| false);
        assert!(
            committed.check_invariant(invariant, &real),
            "{shipping:?} keeps {invariant}: {real:?}"
        );
    }
}
