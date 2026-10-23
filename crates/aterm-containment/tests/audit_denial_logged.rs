// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Integration test: containment gate denials emit audit log events.

use std::sync::{Mutex, OnceLock};

use aterm_containment::ContainmentMode;

static SERIAL: Mutex<()> = Mutex::new(());

struct CapturedRecord {
    target: String,
    level: aterm_log::Level,
    message: String,
}

struct TestLogger;

static CAPTURED: OnceLock<Mutex<Vec<CapturedRecord>>> = OnceLock::new();

fn captured() -> &'static Mutex<Vec<CapturedRecord>> {
    CAPTURED.get_or_init(|| Mutex::new(Vec::new()))
}

impl aterm_log::Log for TestLogger {
    fn enabled(&self, _metadata: &aterm_log::Metadata) -> bool {
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
    let _ = aterm_log::set_logger(&LOGGER);
    aterm_log::set_max_level(aterm_log::LevelFilter::Warn);

    captured().lock().unwrap().clear();

    aterm_containment::log_denial(
        "process",
        "spawn '/bin/bash'",
        ContainmentMode::Containment,
        "NoFork",
    );

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
    let _ = aterm_log::set_logger(&LOGGER);
    aterm_log::set_max_level(aterm_log::LevelFilter::Warn);

    captured().lock().unwrap().clear();

    for mode in [
        ContainmentMode::Containment,
        ContainmentMode::Safety,
        ContainmentMode::User,
    ] {
        aterm_containment::log_denial("test", "op", mode, "reason");
    }

    let records = captured().lock().unwrap();
    assert_eq!(records.len(), 3, "expected one record per mode");

    for record in records.iter() {
        assert_eq!(record.target, "containment_audit");
    }

    assert!(records[0].message.contains("Containment"));
    assert!(records[1].message.contains("Safety"));
    assert!(records[2].message.contains("User"));
}

#[test]
fn log_posture_is_neutral_not_denial() {
    let _lock = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let _ = aterm_log::set_logger(&LOGGER);
    aterm_log::set_max_level(aterm_log::LevelFilter::Info);

    captured().lock().unwrap().clear();

    aterm_containment::log_posture(
        "spawn",
        "os-network-sandbox",
        ContainmentMode::Containment,
        "OS sandbox applied",
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
    let _lock = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let _ = aterm_log::set_logger(&LOGGER);
    aterm_log::set_max_level(aterm_log::LevelFilter::Info);

    captured().lock().unwrap().clear();

    let sandboxed = aterm_containment::os_sandbox_actuated();
    for mode in [
        ContainmentMode::Containment,
        ContainmentMode::Safety,
        ContainmentMode::User,
    ] {
        let decision = aterm_containment::decide_spawn(mode);
        let expected = mode != ContainmentMode::Containment || sandboxed;
        assert_eq!(
            decision.is_permitted(),
            expected,
            "{mode} is permitted iff it needs no OS sandbox or this platform has one \
             (os_sandbox_actuated = {sandboxed}); got {decision:?}"
        );
    }

    let records = captured().lock().unwrap();
    assert_eq!(records.len(), 3, "expected one audit record per decision");
    let denied: Vec<&CapturedRecord> = records
        .iter()
        .filter(|r| r.message.contains("DENIED:"))
        .collect();
    assert_eq!(
        denied.len(),
        usize::from(!sandboxed),
        "only a refused Containment spawn is logged as DENIED (os_sandbox_actuated = {sandboxed})"
    );
    for record in &denied {
        assert_eq!(record.level, aterm_log::Level::Warn, "a denial is Warn");
        assert!(
            record
                .message
                .contains(aterm_containment::NO_OS_SANDBOX_REASON),
            "the refusal names the platform gap: {}",
            record.message
        );
    }
    for record in records.iter() {
        assert_eq!(record.target, "containment_audit", "wrong log target");
        assert!(
            record.message.contains("CONTAINMENT:") || record.message.contains("DENIED:"),
            "every audit line carries its prefix: {}",
            record.message
        );
    }
}

#[test]
fn posture_claims_resource_limits_only_for_the_mode_that_has_them() {
    let _lock = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let _ = aterm_log::set_logger(&LOGGER);
    aterm_log::set_max_level(aterm_log::LevelFilter::Info);

    for (mode, hardened) in [
        (ContainmentMode::User, false),
        (ContainmentMode::Safety, true),
    ] {
        captured().lock().unwrap().clear();
        assert!(
            aterm_containment::decide_spawn(mode).is_permitted(),
            "{mode}"
        );
        let records = captured().lock().unwrap();
        assert_eq!(records.len(), 1, "one posture line for {mode}");
        let message = &records[0].message;
        assert!(message.contains("OS sandbox not applied"), "{message}");
        assert_eq!(
            message.contains("resource limits + process-cap gate only"),
            hardened,
            "{message}"
        );
        assert_eq!(
            message.contains("resource limits inherited from aterm's own process"),
            !hardened,
            "{message}"
        );
    }
    if aterm_containment::os_sandbox_actuated() {
        captured().lock().unwrap().clear();
        let decision = aterm_containment::decide_spawn(ContainmentMode::Containment);
        let aterm_containment::SpawnDecision::Permit { home_scoped, .. } = decision else {
            panic!("a sandboxed Containment spawn is permitted: {decision:?}");
        };
        let records = captured().lock().unwrap();
        assert_eq!(records.len(), 1, "one posture line for Containment");
        let message = &records[0].message;
        assert!(
            message.contains(aterm_containment::applied_posture(home_scoped)),
            "{message}"
        );
    }
}
