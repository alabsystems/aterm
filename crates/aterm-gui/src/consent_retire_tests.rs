// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use std::time::{Duration, Instant};

const DEV_ID: &str = "designated => identifier \"x\" and anchor apple generic and \
                      certificate leaf[subject.OU] = \"T\"";

fn fake_census(_id: &str) -> Claimants {
    let claimant = |path: &str, dr: &str, running: bool| consent::Claimant {
        path: PathBuf::from(path),
        dr: consent::classify_dr(dr),
        dr_text: dr.to_string(),
        signing: if running { "developer-id" } else { "adhoc" },
        team: None,
        running,
        links: Vec::new(),
    };
    Claimants {
        found: vec![
            claimant("/A/x.app", DEV_ID, true),
            claimant("/A/x.app.rollback", "designated => cdhash H\"aa\"", false),
        ],
        enumeration: consent::Enumeration::Complete,
    }
}

fn same(_copy: &consent::Claimant, _bundle_id: &str) -> bool {
    true
}

fn idle(_path: &Path) -> Option<bool> {
    Some(false)
}

fn fake_trash(_path: &Path) -> Result<(), String> {
    Ok(())
}

fn wait_for_report(state: &RetireState) -> RetireReport {
    let deadline = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
    while state.is_live() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    state.report().expect("the worker stored a report")
}

#[test]
fn the_worker_moves_what_a_fresh_census_still_offers_and_reports_it() {
    let state = RetireState::with_arms(RetireArms {
        census: fake_census,
        unchanged: same,
        in_use: idle,
        trash: fake_trash,
    });
    let (tx, rx) = std::sync::mpsc::channel();
    let plan = vec![
        PathBuf::from("/A/x.app.rollback"),
        PathBuf::from("/A/x.app"),
    ];
    assert!(state.start("x".to_string(), plan, move || tx.send(()).unwrap()));
    rx.recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .expect("the worker wakes the loop");
    assert_eq!(
        wait_for_report(&state),
        vec![
            (PathBuf::from("/A/x.app.rollback"), Retired::Moved),
            (PathBuf::from("/A/x.app"), Retired::NotOffered),
        ]
    );
    assert!(state.take_arrival());
    assert!(!state.take_arrival(), "one arrival per report");
}

#[test]
fn the_inert_arms_move_nothing() {
    let state = RetireState::new(true);
    assert!(state.start(
        "x".to_string(),
        vec![PathBuf::from("/A/x.app.rollback")],
        || {}
    ));
    assert_eq!(
        wait_for_report(&state),
        vec![(PathBuf::from("/A/x.app.rollback"), Retired::NotOffered)]
    );
}

#[test]
fn a_second_press_while_live_starts_nothing() {
    static OPEN: (Mutex<bool>, std::sync::Condvar) = (Mutex::new(false), std::sync::Condvar::new());
    fn held_census(id: &str) -> Claimants {
        let (open, opened) = &OPEN;
        let mut open = open
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while !*open {
            open = opened
                .wait(open)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        fake_census(id)
    }
    let state = RetireState::with_arms(RetireArms {
        census: held_census,
        unchanged: same,
        in_use: idle,
        trash: fake_trash,
    });
    assert!(state.start("x".to_string(), Vec::new(), || {}));
    assert!(!state.start("x".to_string(), Vec::new(), || {}));
    *OPEN
        .0
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = true;
    OPEN.1.notify_all();
    let _ = wait_for_report(&state);
}
