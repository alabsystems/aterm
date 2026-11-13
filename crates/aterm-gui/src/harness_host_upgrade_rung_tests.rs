// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn worker() -> (WorkerIdle, Arc<Mutex<Option<upgrade::Rung>>>) {
    let world = Arc::new(World::default());
    let rung = Arc::new(Mutex::new(Some(upgrade::Rung::Prefer)));
    let seen = Arc::clone(&rung);
    let mut h = hooks(&world, parking_body(&world));
    h.acts = Acts {
        due: Arc::new(|_| Due::Yes),
        step: Arc::new(move |_, _, _| UpgradeStep {
            word: "wait:settling".to_string(),
            rung: *seen.lock().unwrap(),
        }),
        ..Acts::inert()
    };
    (
        idle_host("s-rung", Program::Claude, switched(&on()), h),
        rung,
    )
}

fn look(idle: &WorkerIdle) -> bool {
    let step = idle
        .at_idle_on(IdlePoint::default())
        .expect("a settle look");
    assert_eq!(step.line, "upgrade step=wait:settling");
    assert!(!step.moved, "only the turn-end hold changed");
    *idle.look_at.lock().unwrap() = None;
    idle.owns_turn_end()
}

#[test]
fn a_higher_gate_rung_holds_fresh_turn_ends_once() {
    let (idle, rung) = worker();
    for _ in 0..upgrade_drive::QUIET_SETTLE_POINTS {
        assert!(look(&idle));
        idle.turn_ran();
    }
    assert!(!look(&idle), "prefer's turn-end budget was spent");
    idle.turn_ran();
    *rung.lock().unwrap() = Some(upgrade::Rung::Settled);
    for _ in 0..upgrade_drive::QUIET_SETTLE_POINTS {
        assert!(look(&idle), "settled grants its own bounded turn ends");
        idle.turn_ran();
    }
    for same_or_unknown in [
        Some(upgrade::Rung::Settled),
        None,
        Some(upgrade::Rung::Prefer),
        Some(upgrade::Rung::Settled),
    ] {
        *rung.lock().unwrap() = same_or_unknown;
        assert!(!look(&idle), "{same_or_unknown:?} gives no new budget");
        idle.turn_ran();
    }
    *rung.lock().unwrap() = Some(upgrade::Rung::KeysOnly);
    assert!(look(&idle), "the next actual rise grants another budget");
}

#[test]
fn a_rung_rise_keeps_this_points_looks_and_counts_its_hold() {
    let (idle, rung) = worker();
    for _ in 0..upgrade_drive::OWNED_SETTLE_LOOKS {
        assert!(look(&idle));
    }
    *rung.lock().unwrap() = Some(upgrade::Rung::Settled);
    assert!(!look(&idle), "a rise cannot hold this point past its looks");
    idle.turn_ran();
    for _ in 1..upgrade_drive::QUIET_SETTLE_POINTS {
        assert!(look(&idle));
        idle.turn_ran();
    }
    assert!(
        !look(&idle),
        "the point held at the rise counted in its budget"
    );
}

#[test]
fn a_fallback_report_carries_no_other_tabs_rung() {
    let report = |tab: &str| upgrade_drive::Report {
        rung: Some(upgrade::Rung::Settled),
        pid: 42,
        tab: tab.to_string(),
        session: "conversation".to_string(),
        from: "1.0.0".to_string(),
        to: "2.0.0".to_string(),
        step: "wait:settling".to_string(),
    };
    assert_eq!(
        UpgradeStep::reported(report("s-own"), "s-own").rung,
        Some(upgrade::Rung::Settled)
    );
    assert_eq!(UpgradeStep::reported(report("s-peer"), "s-own").rung, None);
}
