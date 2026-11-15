// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn natural() -> Config {
    Config {
        start_file: "start".into(),
        events_file: "events.jsonl".into(),
        wait_uri: None,
        release_file: None,
        closed_reopen: false,
        journal_release_file: None,
        seed_closed_reopen: false,
        seed_closed_view: false,
    }
}

#[test]
fn private_measurement_config_requires_distinct_local_names_and_a_complete_wait() {
    assert!(natural().validate().is_ok());
    for invalid in ["../start", "/tmp/start", "a/start", ".", ""] {
        let mut config = natural();
        config.start_file = invalid.into();
        assert!(config.validate().is_err(), "{invalid}");
    }
    let mut config = natural();
    config.events_file = "start".into();
    assert!(config.validate().is_err());
    config = natural();
    config.wait_uri = Some("file:///tmp/fixture.txt".into());
    assert!(config.validate().is_err());
    config.release_file = Some("release".into());
    assert!(config.validate().is_ok());
    config.release_file = Some("start".into());
    assert!(config.validate().is_err());
    assert!(
        aterm_json::from_str::<Config>(
            r#"{"start_file":"start","events_file":"events","unknown":true}"#
        )
        .is_err()
    );
}

#[test]
fn gate_admits_only_the_exact_release_token() {
    let root = aterm_tempfile::tempdir().unwrap();
    let directory = PrivateDirectory::open(root.path()).unwrap();
    assert!(!read_gate(&directory, "start", b"start\n").unwrap());
    directory
        .view()
        .unwrap()
        .write("start", b"release\n")
        .unwrap();
    assert!(read_gate(&directory, "start", b"start\n").is_err());
    directory
        .view()
        .unwrap()
        .write("start", b"start\n")
        .unwrap();
    assert!(read_gate(&directory, "start", b"start\n").unwrap());
}

#[test]
fn callback_ledger_keeps_the_largest_completed_span_and_all_recorded_returns() {
    let stats = CallbackStats::default();
    stats.record(10, 15, false);
    stats.record(20, 48, true);
    stats.record(60, 70, false);
    assert_eq!(stats.count.load(Ordering::Relaxed), 3);
    assert_eq!(stats.total.load(Ordering::Relaxed), 43);
    assert_eq!(stats.first.load(Ordering::Relaxed), 10);
    assert_eq!(stats.last.load(Ordering::Relaxed), 70);
    assert_eq!(stats.max.load(Ordering::Relaxed), 28);
    assert_eq!(
        (
            stats.max_begin.load(Ordering::Relaxed),
            stats.max_end.load(Ordering::Relaxed)
        ),
        (20, 48)
    );
    assert!(stats.finished.load(Ordering::Relaxed));
    stats.reset();
    assert_eq!(stats.count.load(Ordering::Relaxed), 0);
    assert_eq!(stats.max.load(Ordering::Relaxed), 0);
    assert!(!stats.finished.load(Ordering::Relaxed));
}

#[test]
fn each_hardware_burst_has_its_own_actual_post_bounds() {
    let stats = KeyStats::default();
    let warmup = stats.begin();
    stats.record(warmup, 10, 11);
    stats.end(warmup);
    let measured = stats.begin();
    assert_ne!(warmup, measured);
    assert_eq!(stats.first.load(Ordering::Relaxed), 0);
    stats.record(measured, 50, 52);
    stats.record(measured, 90, 94);
    stats.record(warmup, 100, 101);
    assert_eq!(stats.count.load(Ordering::Relaxed), 2);
    assert_eq!(
        (
            stats.first.load(Ordering::Relaxed),
            stats.last.load(Ordering::Relaxed)
        ),
        (50, 94)
    );
    stats.end(measured);
    assert!(!stats.active.load(Ordering::Relaxed));
    assert!(!stats.conflict.load(Ordering::Relaxed));
    let interrupted = stats.begin();
    let current = stats.begin();
    stats.end(interrupted);
    assert!(stats.active.load(Ordering::Relaxed));
    assert!(stats.conflict.load(Ordering::Relaxed));
    stats.end(current);
}

#[test]
fn closed_view_seed_requires_the_complete_private_closed_history_fixture() {
    let mut config = natural();
    config.seed_closed_view = true;
    assert!(config.validate().is_err());
    config.seed_closed_reopen = true;
    assert!(config.validate().is_err());
    config.closed_reopen = true;
    config.wait_uri = Some("file:///tmp/closed-view.txt".into());
    config.release_file = Some("release".into());
    config.journal_release_file = Some("journal-release".into());
    assert!(config.validate().is_ok());
}

fn closed_fixture_measurement(directory: PrivateDirectory, view: bool) -> Arc<Measurement> {
    let mut config = natural();
    config.closed_reopen = true;
    config.seed_closed_reopen = true;
    config.seed_closed_view = view;
    config.wait_uri = Some("file:///tmp/private-closed-fixture.txt".into());
    config.release_file = Some("release".into());
    config.journal_release_file = Some("journal-release".into());
    config.validate().unwrap();
    Arc::new(Measurement::new(config, directory))
}

#[test]
fn fresh_fixture_setup_needs_successful_release_and_a_live_anchor_then_seeds_once() {
    for view in [false, true] {
        let root = aterm_tempfile::tempdir().unwrap();
        let measurement =
            closed_fixture_measurement(PrivateDirectory::open(root.path()).unwrap(), view);
        let loaded = Loaded::Ready(Arc::clone(&measurement));
        let mut app = crate::App::headless_for_test();
        assert!(!app.restore_pass_queued());
        let uri = measurement.config.wait_uri.as_ref().unwrap();
        assert!(app.document_store.id_for_uri(uri).is_none());
        seed_released_fixture(&mut app, &loaded);
        assert!(!measurement.fixture_taken.load(Ordering::Acquire));
        let frontmost = app.frontmost_window.take();
        measurement.gate.store(RELEASED, Ordering::Release);
        seed_released_fixture(&mut app, &loaded);
        assert!(!measurement.fixture_taken.load(Ordering::Acquire));
        app.frontmost_window = frontmost;
        seed_released_fixture(&mut app, &loaded);
        assert!(measurement.fixture_taken.load(Ordering::Acquire));
        assert!(!measurement.failed.load(Ordering::Acquire));
        let candidate = |app: &crate::App| {
            if view {
                app.closed_recovery
                    .views
                    .candidate_snapshot(0)
                    .map(|candidate| candidate.token)
            } else {
                app.closed_recovery
                    .tabs
                    .candidate_snapshot(0)
                    .map(|candidate| candidate.token)
            }
        };
        let token = candidate(&app).expect("released fresh setup seeded history");
        seed_released_fixture(&mut app, &loaded);
        assert_eq!(candidate(&app), Some(token));
        assert!(app.document_store.id_for_uri(uri).is_none());
        assert!(!app.restore_pass_queued());
        let events = measurement.events.lock().unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|event| event.event == "reopen_fixture_seeded")
                .count(),
            1
        );
    }
}

#[test]
fn failed_refused_and_ordinary_restore_setup_never_seed_closed_history() {
    let root = aterm_tempfile::tempdir().unwrap();
    let measurement =
        closed_fixture_measurement(PrivateDirectory::open(root.path()).unwrap(), false);
    let loaded = Loaded::Ready(Arc::clone(&measurement));
    let mut app = crate::App::headless_for_test();
    for gate in [PENDING, FAILED] {
        measurement.gate.store(gate, Ordering::Release);
        seed_released_fixture(&mut app, &loaded);
        assert!(!measurement.fixture_taken.load(Ordering::Acquire));
    }
    measurement.gate.store(RELEASED, Ordering::Release);
    measurement.failed.store(true, Ordering::Release);
    seed_released_fixture(&mut app, &loaded);
    assert!(!measurement.fixture_taken.load(Ordering::Acquire));
    seed_released_fixture(&mut app, &Loaded::Refused);
    seed_released_fixture(&mut app, &Loaded::Absent);
    let ordinary = Arc::new(Measurement::new(
        natural(),
        PrivateDirectory::open(root.path()).unwrap(),
    ));
    ordinary.gate.store(RELEASED, Ordering::Release);
    seed_released_fixture(&mut app, &Loaded::Ready(Arc::clone(&ordinary)));
    assert!(!ordinary.fixture_taken.load(Ordering::Acquire));
    assert!(app.closed_recovery.tabs.candidate_snapshot(0).is_none());
    assert!(app.closed_recovery.views.candidate_snapshot(0).is_none());
    assert!(measurement.events.lock().unwrap().is_empty());
    assert!(ordinary.events.lock().unwrap().is_empty());
}
