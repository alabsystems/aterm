// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Integration test: containment gate denials emit audit log events (#5533).
//!
//! Installs a custom logger that captures log messages, calls `log_denial()`,
//! and asserts the expected structured output is captured with the correct
//! `containment_audit` target.

use std::sync::{Mutex, OnceLock};

use aterm_containment::ContainmentMode;

/// Mutex to serialize tests that share a process-global logger (#7698).
static SERIAL: Mutex<()> = Mutex::new(());

/// Captured log record.
struct CapturedRecord {
    target: String,
    level: aterm_log::Level,
    message: String,
}

/// Test logger that captures records into a global Vec.
struct TestLogger;

static CAPTURED: OnceLock<Mutex<Vec<CapturedRecord>>> = OnceLock::new();

fn captured() -> &'static Mutex<Vec<CapturedRecord>> {
    CAPTURED.get_or_init(|| Mutex::new(Vec::new()))
}

impl aterm_log::Log for TestLogger {
    fn enabled(&self, _metadata: &aterm_log::Metadata<'_>) -> bool {
        true
    }

    fn log(&self, record: &aterm_log::Record<'_>) {
        captured().lock().unwrap().push(CapturedRecord {
            target: record.target().to_string(),
            level: record.level(),
            message: format!("{}", record.args()),
        });
    }

    fn flush(&self) {}
}

static LOGGER: TestLogger = TestLogger;

#[test]
fn log_denial_emits_audit_event() {
    let _lock = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    // Install test logger (once per binary — may race with other tests in this file).
    let _ = aterm_log::set_logger(&LOGGER);
    aterm_log::set_max_level(aterm_log::LevelFilter::Warn);

    // Clear any records from other tests that ran first.
    captured().lock().unwrap().clear();

    // Emit a denial event.
    aterm_containment::log_denial(
        "process",
        "spawn '/bin/bash'",
        ContainmentMode::Containment,
        "NoFork",
    );

    // Verify captured record.
    let records = captured().lock().unwrap();
    assert_eq!(records.len(), 1, "expected exactly one log record");

    let record = &records[0];
    assert_eq!(record.target, "containment_audit", "wrong log target");
    assert_eq!(record.level, aterm_log::Level::Warn, "wrong log level");
    assert!(
        record.message.contains("DENIED:"),
        "message should start with DENIED: got: {}",
        record.message
    );
    assert!(
        record.message.contains("process"),
        "message should include subsystem: {}",
        record.message
    );
    assert!(
        record.message.contains("spawn '/bin/bash'"),
        "message should include operation: {}",
        record.message
    );
    assert!(
        record.message.contains("Containment"),
        "message should include mode: {}",
        record.message
    );
    assert!(
        record.message.contains("NoFork"),
        "message should include reason: {}",
        record.message
    );
}

#[test]
fn log_denial_includes_all_modes() {
    let _lock = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    // Install test logger (may already be set by the other test — that's fine,
    // both tests run in the same binary but the OnceLock captures all).
    let _ = aterm_log::set_logger(&LOGGER);
    aterm_log::set_max_level(aterm_log::LevelFilter::Warn);

    // Clear captured records.
    captured().lock().unwrap().clear();

    // Emit denials for each mode.
    for mode in [
        ContainmentMode::Containment,
        ContainmentMode::Safety,
        ContainmentMode::User,
        ContainmentMode::Master,
    ] {
        aterm_containment::log_denial("test", "op", mode, "reason");
    }

    let records = captured().lock().unwrap();
    assert_eq!(records.len(), 4, "expected one record per mode");

    // All use the same target.
    for record in records.iter() {
        assert_eq!(record.target, "containment_audit");
    }

    // Each mentions its mode.
    assert!(records[0].message.contains("Containment"));
    assert!(records[1].message.contains("Safety"));
    assert!(records[2].message.contains("User"));
    assert!(records[3].message.contains("Master"));
}

#[test]
fn log_posture_is_neutral_not_denial() {
    let _lock = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let _ = aterm_log::set_logger(&LOGGER);
    // Posture events are Info — below the Warn denials — so capture at Info.
    aterm_log::set_max_level(aterm_log::LevelFilter::Info);

    captured().lock().unwrap().clear();

    // A posture/permit event must share the audit target but NOT be a DENIED line.
    aterm_containment::log_posture(
        "spawn",
        "os-network-sandbox",
        ContainmentMode::Containment,
        "OS sandbox ACTUATED",
    );

    let records = captured().lock().unwrap();
    assert_eq!(records.len(), 1, "expected exactly one log record");
    let record = &records[0];
    assert_eq!(record.target, "containment_audit", "wrong log target");
    assert_eq!(record.level, aterm_log::Level::Info, "posture is Info");
    assert!(
        !record.message.contains("DENIED:"),
        "posture must NOT carry the DENIED: prefix (it is not a denial): {}",
        record.message
    );
    assert!(
        record.message.contains("CONTAINMENT:"),
        "posture should carry the CONTAINMENT: prefix: {}",
        record.message
    );
}

#[test]
fn decide_permit_is_not_logged_as_denied() {
    // Regression (#5533 follow-up): every spawn flows through `decide`, which
    // records the OS-sandbox posture. A PERMITTED spawn must never surface in the
    // audit stream as `DENIED:` — otherwise a `containment_audit | grep DENIED`
    // audit aggregation false-positives on every successful/permitted spawn.
    let _lock = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let _ = aterm_log::set_logger(&LOGGER);
    // Capture both posture (Info) and any denial (Warn) lines.
    aterm_log::set_max_level(aterm_log::LevelFilter::Info);

    captured().lock().unwrap().clear();

    for mode in [
        ContainmentMode::Containment,
        ContainmentMode::Safety,
        ContainmentMode::User,
        ContainmentMode::Master,
    ] {
        let decision = aterm_containment::decide_spawn(mode);
        assert!(
            decision.is_permitted(),
            "the initial shell is permitted in {mode} mode; got {decision:?}"
        );
    }

    let records = captured().lock().unwrap();
    // A posture line per mode is still emitted (single-stream coverage preserved).
    assert_eq!(
        records.len(),
        4,
        "expected one posture record per permitted spawn"
    );
    for record in records.iter() {
        assert_eq!(record.target, "containment_audit", "wrong log target");
        assert!(
            !record.message.contains("DENIED:"),
            "a PERMITTED spawn must NOT be logged as DENIED: {}",
            record.message
        );
        assert!(
            record.message.contains("CONTAINMENT:"),
            "posture line should carry the CONTAINMENT: prefix: {}",
            record.message
        );
    }
}
