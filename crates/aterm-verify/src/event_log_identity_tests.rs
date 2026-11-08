// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn row(pad: &str) -> String {
    format!(
        "verify-event: {}",
        aterm_json::json!({
            "schema":1, "event":"check-finished", "check":"doc", "outcome":"ok",
            "detail":"", "seconds":0.0, "rerun":"", "findings":[], "findings_total":0,
            "rows":[], "rows_total":0, "pad":pad,
        })
    )
}

#[test]
fn independent_handles_and_hardlink_aliases_lock_the_actual_append_file() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join("events");
    let first = EventLog::open(&path).unwrap();
    let alias = dir.path().join("alias");
    std::fs::hard_link(&path, &alias).unwrap();
    let second = EventLog::open(&alias).unwrap();
    let holder = EventLog::open(&path)
        .unwrap()
        .sinks
        .into_inner()
        .unwrap()
        .external
        .unwrap();
    aterm_uds::file_lock::lock_exclusive_until(&holder, Instant::now(), "busy").unwrap();
    let holder = Arc::new(Mutex::new(Some(holder)));
    let observed = Arc::new(AtomicBool::new(false));
    let release = Arc::clone(&holder);
    let witness = Arc::clone(&observed);
    aterm_uds::file_lock::with_lock_wait_observer(
        move |_| {
            if let Some(file) = release.lock().unwrap().take() {
                witness.store(true, Ordering::Relaxed);
                file.unlock().unwrap();
            }
        },
        || second.write(&row("alias")),
    );
    first.write(&row("first"));
    assert!(observed.load(Ordering::Relaxed));
    assert!(holder.lock().unwrap().is_none());
    let text = std::fs::read_to_string(&path).unwrap();
    let groups = crate::status::observe(&text);
    assert_eq!(groups.invocations.len(), 2);
    assert!(groups.unattributed.is_none());
    assert_ne!(first.id(), second.id());
    assert!(
        groups
            .invocations
            .iter()
            .all(|run| run.observed.checks[0].occurrences == 1)
    );
}

#[test]
fn concurrent_event_writer_stand_in() {
    let Some(path) = std::env::var_os("ATERM_VERIFY_EVENT_WRITER") else {
        return;
    };
    let world = std::path::PathBuf::from(std::env::var_os("ATERM_VERIFY_EVENT_WORLD").unwrap());
    let label = std::env::var("ATERM_VERIFY_EVENT_LABEL").unwrap();
    let log = EventLog::open(Path::new(&path)).unwrap();
    std::fs::write(world.join(format!("{label}.ready")), b"ready").unwrap();
    let until = Instant::now() + Duration::from_secs(60);
    while !world.join("go").exists() && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(
        world.join("go").exists(),
        "parent never released owned writers"
    );
    for _ in 0..20 {
        log.write(&row(&label.repeat(20_000)));
    }
}

#[test]
fn two_actual_processes_keep_overlapping_check_names_in_distinct_invocations() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let path = dir.path().join("events");
    let spawn = |label| {
        let mut child = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
        child
            .args([
                "--exact",
                "event_log::identity_tests::concurrent_event_writer_stand_in",
                "--nocapture",
            ])
            .env("ATERM_VERIFY_EVENT_WRITER", &path)
            .env("ATERM_VERIFY_EVENT_WORLD", dir.path())
            .env("ATERM_VERIFY_EVENT_LABEL", label)
            .stdout(aterm_uds::spawnfd::Stdio::null())
            .stderr(aterm_uds::spawnfd::Stdio::null());
        child.spawn()
    };
    let mut first = spawn("a").unwrap();
    let mut second = spawn("b");
    let until = Instant::now() + Duration::from_secs(60);
    while second.is_ok()
        && !(dir.path().join("a.ready").exists() && dir.path().join("b.ready").exists())
        && Instant::now() < until
    {
        std::thread::sleep(Duration::from_millis(1));
    }
    let both_ready = dir.path().join("a.ready").exists() && dir.path().join("b.ready").exists();
    let released = std::fs::write(dir.path().join("go"), b"go");
    let mut first_status = None;
    let mut second_status = None;
    let mut wait_failed = false;
    loop {
        if first_status.is_none() {
            match first.try_wait() {
                Ok(status) => first_status = status,
                Err(_) => wait_failed = true,
            }
        }
        if let Ok(child) = &mut second
            && second_status.is_none()
        {
            match child.try_wait() {
                Ok(status) => second_status = status,
                Err(_) => wait_failed = true,
            }
        }
        if (first_status.is_some() && second_status.is_some())
            || wait_failed
            || second.is_err()
            || Instant::now() >= until
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    // Cleanup uses the existing owning settle operation on BOTH roots, even on timeout or a
    // wait error. It never releases unknown custody merely because the work deadline passed.
    aterm_uds::deadline::settle_child(&mut first);
    if let Ok(child) = &mut second {
        aterm_uds::deadline::settle_child(child);
    }
    assert!(released.is_ok());
    assert!(both_ready);
    assert!(second.is_ok());
    assert!(!wait_failed);
    assert!(first_status.is_some_and(|status| status.success()));
    assert!(second_status.is_some_and(|status| status.success()));
    let text = std::fs::read_to_string(&path).unwrap();
    assert_eq!(text.lines().count(), 40);
    let observed = crate::status::observe(&text);
    assert_eq!(observed.malformed_complete_records, 0);
    assert_eq!(observed.invocations.len(), 2);
    assert!(
        observed
            .invocations
            .iter()
            .all(|run| run.observed.checks[0].occurrences == 20)
    );
    assert!(
        observed
            .invocations
            .iter()
            .all(|run| run.source_observation.is_none())
    );
}

#[test]
fn a_bounded_descriptor_and_oversized_diagnostic_remain_an_unknown_observation() {
    let ctx = crate::Ctx::new(
        std::path::PathBuf::from("/fixture"),
        crate::Scope::crate_only("x".repeat(100_000)),
        crate::EnvSnapshot::default(),
        std::path::PathBuf::from("/fixture/scratch"),
    )
    .with_checks(&vec![crate::Check::Doc; 1000]);
    let mut log = EventLog::unrecorded();
    log.bind(Context::captured(&ctx));
    let prepared = log.prepare(&row(&"\0\\\"界".repeat(20_000)));
    assert!(prepared.len() <= 64 << 10);
    let value: aterm_json::Value =
        aterm_json::from_str(prepared.trim().strip_prefix("verify-event: ").unwrap()).unwrap();
    assert_eq!(value["event"].as_str(), Some("observation-unavailable"));
    assert!(
        value["invocation"]["source_observation"]["selection_truncated"]
            .as_bool()
            .unwrap()
    );
    assert!(
        value["invocation"]["source_observation"]["checks"]
            .as_array()
            .unwrap()
            .len()
            <= 32
    );
    let observed = crate::status::observe(&prepared);
    assert_eq!(observed.invocations.len(), 1);
    assert!(observed.invocations[0].observed.checks.is_empty());
    assert!(observed.json()["source_identity"].is_null());
}

#[test]
fn unavailable_entropy_stays_unknown_without_a_fallback_identity() {
    let log = EventLog::with_entropy(|| Err(io::Error::other("no entropy")));
    let line = log.prepare(&row(""));
    let value: aterm_json::Value =
        aterm_json::from_str(line.trim().strip_prefix("verify-event: ").unwrap()).unwrap();
    assert!(value["invocation"]["id"].is_null());
    assert_eq!(
        value["invocation"]["identity_error"].as_str(),
        Some("entropy-unavailable")
    );
    let observed = crate::status::observe(&line);
    assert!(observed.invocations.is_empty());
    assert_eq!(observed.unattributed.unwrap().checks.len(), 1);
}
