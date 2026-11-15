// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::cell::Cell;

fn driver() -> Identity {
    Identity {
        pid: 71,
        birth: aterm_uds::process::Birth(123),
        idversion: None,
    }
}

#[test]
fn setup_gate_follows_the_live_driver_while_a_producer_reply_still_has_a_hang_detector() {
    for awaiting in [Await::Gate(driver()), Await::Reply] {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let files = Files {
            dir: std::fs::canonicalize(tmp.path()).unwrap(),
        };
        let began = Instant::now();
        let elapsed = Cell::new(Duration::ZERO);
        let result = files.wait_with(
            "start",
            awaiting,
            || began + elapsed.get(),
            |_| Some(driver()),
            |duration| {
                if duration.is_zero() {
                    return Ok(());
                }
                elapsed.set(elapsed.get() + Duration::from_secs(31));
                if elapsed.get() >= Duration::from_secs(93) {
                    files.gate("start")?;
                }
                Ok(())
            },
        );
        match awaiting {
            Await::Gate(_) => {
                assert_eq!(result.unwrap(), "go\n");
                assert!(elapsed.get() > HANG);
            }
            Await::Reply => assert_eq!(result.unwrap_err(), "producer never reached start"),
        }
    }
}

#[test]
fn a_missing_or_reused_driver_cannot_open_a_gate_even_when_its_file_exists() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let files = Files {
        dir: std::fs::canonicalize(tmp.path()).unwrap(),
    };
    files.gate("start").unwrap();
    let reused = Identity {
        birth: aterm_uds::process::Birth(driver().birth.0 + 1),
        ..driver()
    };
    for observed in [None, Some(reused)] {
        let error = files
            .wait_with(
                "start",
                Await::Gate(driver()),
                Instant::now,
                |_| observed,
                |duration| {
                    assert_eq!(duration, Duration::ZERO);
                    Ok(())
                },
            )
            .unwrap_err();
        assert_eq!(
            error,
            "title-drift driver identity unavailable or changed before start"
        );
    }
}

#[test]
fn a_waiting_gate_propagates_interruption() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let files = Files {
        dir: std::fs::canonicalize(tmp.path()).unwrap(),
    };
    let error = files
        .wait_with(
            "clear",
            Await::Gate(driver()),
            Instant::now,
            |_| Some(driver()),
            |_| Err("interrupted by signal 15".into()),
        )
        .unwrap_err();
    assert_eq!(error, "interrupted by signal 15");
}

#[test]
fn pending_interruption_wins_over_an_existing_gate_or_reply() {
    let tmp = aterm_tempfile::tempdir().unwrap();
    let files = Files {
        dir: std::fs::canonicalize(tmp.path()).unwrap(),
    };
    files.gate("start").unwrap();
    for awaiting in [Await::Gate(driver()), Await::Reply] {
        let error = files
            .wait_with(
                "start",
                awaiting,
                Instant::now,
                |_| panic!("a pending interruption must precede reading the driver identity"),
                |duration| {
                    assert_eq!(duration, Duration::ZERO);
                    Err("interrupted by signal 15".into())
                },
            )
            .unwrap_err();
        assert_eq!(error, "interrupted by signal 15");
    }
}
