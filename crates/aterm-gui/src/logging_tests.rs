// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::control_auth::ensure_private_dir;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

#[test]
fn the_logs_sweeps_leave_the_session_journal_alone() {
    let dir = Scratch::new("journal");
    let journal = dir.0.path().join("session-log");
    std::fs::create_dir(&journal).unwrap();
    let seg = journal.join("p1-aa.1.slog");
    std::fs::write(
        &seg,
        b"s1\t*\t#seg\tv=1\tinst=p1-aa\tn=1\tt=1\tbuild=x\tend=\n",
    )
    .unwrap();
    std::fs::write(journal.join("p1-aa.lock"), b"").unwrap();
    for i in 0..20 {
        std::fs::write(dir.0.path().join(format!("crash-{i}.log.seen")), b"x").unwrap();
        std::fs::write(dir.0.path().join(format!("harness-fault-{i}.log")), b"x").unwrap();
    }
    prune_seen_crash_reports(dir.0.path(), None, None);
    prune_harness_faults(dir.0.path(), std::process::id());
    let mut left: Vec<String> = std::fs::read_dir(&journal)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    left.sort();
    assert_eq!(left, vec!["p1-aa.1.slog", "p1-aa.lock"]);
    assert!(std::fs::read(&seg).unwrap().starts_with(b"s1\t*\t#seg"));
    let crash = std::fs::read_dir(dir.0.path())
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("crash-"))
        .count();
    assert!(crash < 20, "{crash}");
}

struct Scratch(aterm_tempfile::TempDir);

impl Scratch {
    fn new(tag: &str) -> Self {
        let dir = aterm_tempfile::Builder::new()
            .prefix(&format!("aterm-log-{tag}-"))
            .tempdir()
            .unwrap();
        ensure_private_dir(dir.path()).unwrap();
        Self(dir)
    }

    fn log(&self) -> PathBuf {
        self.0.path().join("aterm.log")
    }
}

fn tiny(rotate_at: u64) -> RotationBudget {
    RotationBudget {
        rotate_at,
        check_bytes: 1,
        check_every: Duration::from_secs(30),
    }
}

fn note(older: &Path) -> String {
    format!("NOTE {}\n", older.file_name().unwrap().to_string_lossy())
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

#[cfg(unix)]
#[test]
fn log_file_is_0600_and_appends_across_opens() {
    let dir = Scratch::new("app");
    let path = dir.log();
    RotatingFile::open(path.clone(), RotationBudget::ATERM_LOG)
        .unwrap()
        .append(b"first\n", note);
    RotatingFile::open(path.clone(), RotationBudget::ATERM_LOG)
        .unwrap()
        .append(b"second\n", note);
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    assert_eq!(read(&path), "first\nsecond\n");

    let outside = dir.0.path().join("outside.txt");
    std::fs::write(&outside, b"keep\n").unwrap();
    std::fs::set_permissions(&outside, std::fs::Permissions::from_mode(0o644)).unwrap();
    let link = dir.0.path().join("linked.log");
    std::os::unix::fs::symlink(&outside, &link).unwrap();
    assert!(open_append_0600(&link).is_err());
    assert!(create_0600(&link).is_err());
    assert_eq!(read(&outside), "keep\n");
    assert_eq!(
        std::fs::metadata(&outside).unwrap().permissions().mode() & 0o777,
        0o644
    );
}

#[test]
fn oversized_log_rotates_on_open_and_keeps_the_old_lines() {
    let dir = Scratch::new("rot");
    let path = dir.log();
    open_append_0600(&path)
        .unwrap()
        .write_all(b"before the crash\n")
        .unwrap();
    let mut f = RotatingFile::open(path.clone(), tiny(4)).unwrap();
    assert_eq!(read(&rotated_path(&path)), "before the crash\n");
    assert_eq!(std::fs::metadata(&path).unwrap().len(), 0);
    f.append(b"after\n", note);
    assert_eq!(read(&path), "NOTE aterm.log.1\nafter\n");
    #[cfg(unix)]
    for p in [rotated_path(&path), lock_path(&path)] {
        let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "{}", p.display());
    }
}

#[test]
fn rotates_while_running_and_keeps_one_older_copy() {
    let dir = Scratch::new("run");
    let path = dir.log();
    let mut f = RotatingFile::open(path.clone(), tiny(40)).unwrap();
    for i in 0..40 {
        f.append(format!("line {i:02}\n").as_bytes(), note);
    }
    let mut names: Vec<String> = std::fs::read_dir(dir.0.path())
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names, ["aterm.log", "aterm.log.1", "aterm.log.lock"]);
    for p in [&path, &rotated_path(&path)] {
        let len = std::fs::metadata(p).unwrap().len();
        assert!(len <= 40 + 8, "{} is {len} bytes", p.display());
    }
    assert!(read(&path).ends_with("line 39\n"));
    assert!(read(&path).starts_with("NOTE aterm.log.1\n"));
}

#[test]
fn a_deleted_log_is_recreated_at_the_next_look() {
    let dir = Scratch::new("gone");
    let path = dir.log();
    let mut f = RotatingFile::open(path.clone(), tiny(1024)).unwrap();
    f.append(b"one\n", note);
    std::fs::remove_file(&path).unwrap();
    f.append(b"two\n", note);
    assert_eq!(read(&path), "two\n");
}

#[test]
fn a_busy_lock_skips_the_rotation_without_waiting() {
    let dir = Scratch::new("busy");
    let path = dir.log();
    let mut f = RotatingFile::open(path.clone(), tiny(4)).unwrap();
    let holder = crate::native_document_host::HeldAdvisoryLock::try_lock(
        open_lock_0600(&lock_path(&path)).unwrap(),
    )
    .unwrap();
    for line in ["one\n", "two\n", "three\n"] {
        f.append(line.as_bytes(), note);
    }
    assert!(!rotated_path(&path).exists());
    assert_eq!(read(&path), "one\ntwo\nthree\n");
    drop(holder);
    f.append(b"four\n", note);
    assert_eq!(read(&rotated_path(&path)), "one\ntwo\nthree\n");
    assert_eq!(read(&path), "NOTE aterm.log.1\nfour\n");
}

/// The rotation lock is free the moment a rotation ends, even while a child forked
/// under it has yet to exec.
#[cfg(unix)]
#[test]
fn a_rotation_frees_its_lock_while_a_child_forked_under_it_has_yet_to_exec() {
    let dir = Scratch::new("fork-window");
    let path = dir.log();
    open_append_0600(&path)
        .unwrap()
        .write_all(b"first\n")
        .unwrap();
    let armed = crate::parked_fork::arm_at(1);
    assert_eq!(rotate_if_oversized(&path, 4, None), Rotation::Rotated);
    let child = armed.child();
    assert!(
        child.still_parked(),
        "forked under the first rotation's lock"
    );

    open_append_0600(&path)
        .unwrap()
        .write_all(b"second\n")
        .unwrap();
    assert_eq!(
        rotate_if_oversized(&path, 4, None),
        Rotation::Rotated,
        "the next rotation takes the lock"
    );
    assert!(child.still_parked(), "…while the child had yet to exec");
    child.release();
    assert_eq!(read(&rotated_path(&path)), "second\n");
}

#[test]
fn rotation_never_loses_or_duplicates_a_line() {
    let dir = Scratch::new("once");
    let path = dir.log();
    let mut a = RotatingFile::open(path.clone(), tiny(64)).unwrap();
    let mut b = RotatingFile::open(path.clone(), tiny(64)).unwrap();
    let mut written = Vec::new();
    for i in 0..60 {
        let (w, name) = if i % 3 == 0 {
            (&mut b, "b")
        } else {
            (&mut a, "a")
        };
        let line = format!("{name}{i:03}");
        w.append(format!("{line}\n").as_bytes(), note);
        written.push(line);
    }
    let kept: Vec<String> = [rotated_path(&path), path.clone()]
        .iter()
        .flat_map(|p| read(p).lines().map(str::to_owned).collect::<Vec<_>>())
        .filter(|l| !l.starts_with("NOTE"))
        .collect();
    let mut sorted = kept.clone();
    sorted.sort_by_key(|l| l[1..].parse::<u32>().unwrap());
    sorted.dedup();
    assert_eq!(sorted.len(), kept.len(), "a line was duplicated: {kept:?}");
    let first = written.iter().position(|l| *l == sorted[0]).unwrap();
    assert_eq!(sorted, written[first..], "a line went missing");
    assert!(first > 0, "the test must rotate at least twice");
}

#[cfg(unix)]
#[test]
fn crash_report_carries_version_message_and_backtrace() {
    let dir = std::env::temp_dir().join(format!("aterm-log-crash-{}", std::process::id()));
    ensure_private_dir(&dir).unwrap();
    let path = dir.join("crash-1.log");
    write_crash_report(
        &path,
        false,
        None,
        &"panicked at 'boom', main.rs:7",
        &"0: frame_a",
    )
    .unwrap();
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    let report = std::fs::read_to_string(&path).unwrap();
    assert!(report.contains(crate::build_info::version()));
    assert!(report.contains("panicked at 'boom', main.rs:7"));
    assert!(report.contains("backtrace:\n0: frame_a"));
    assert_eq!(
        panic_report_head(&path),
        ReportHead::default(),
        "a worker's panic is not marked, and a run with no marker names none"
    );
    write_crash_report(
        &path,
        true,
        Some(1_727_000_000_123_456_789),
        &"second",
        &"bt",
    )
    .unwrap();
    assert!(!std::fs::read_to_string(&path).unwrap().contains("boom"));
    assert_eq!(
        panic_report_head(&path),
        ReportHead {
            main_thread: true,
            run: Some(1_727_000_000_123_456_789),
        },
        "the main thread's is marked, and the report names its run"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn panic_report_timestamp_preserves_fractions_and_refuses_malformed_fields() {
    for (field, expected) in [
        ("42", Some(Duration::from_secs(42))),
        ("42.250", Some(Duration::new(42, 250_000_000))),
        ("42.25", Some(Duration::new(42, 250_000_000))),
        ("42.000000001", Some(Duration::new(42, 1))),
        ("42.", None),
        ("+42.250", None),
        ("-42.250", None),
        ("42.250x", None),
        ("42.1234567890", None),
        ("18446744073709551616.000", None),
    ] {
        let first = format!("aterm-gui 0.1.0 crashed at unix {field} in run 7 on the main thread");
        assert_eq!(panic_report_timestamp(&first), expected, "{field}");
    }
    assert_eq!(panic_report_timestamp("aterm: fatal signal 6"), None);
    assert_eq!(
        panic_report_timestamp("panicked at crashed at unix 42.250"),
        None
    );
}

#[test]
fn crash_evidence_returns_the_path_and_the_head_once_and_preserves_the_artifact() {
    let dir = std::env::temp_dir().join(format!("aterm-log-seen-{}", std::process::id()));
    ensure_private_dir(&dir).unwrap();
    std::fs::write(dir.join("crash-signal-4242-7.log"), b"fatal signal 11").unwrap();
    std::fs::write(dir.join("crash-signal-9999-8.log"), b"").unwrap();
    std::fs::write(dir.join("aterm.log"), b"routine line").unwrap();
    let mut report = String::from("aterm-gui 0.1.0 crashed at unix 1.000\n");
    report.push_str("panicked at 'boom', main.rs:7\n\nbacktrace:\n");
    for frame in 0..40 {
        report.push_str(&format!("{frame}: {}\n", "f".repeat(60)));
    }
    std::fs::write(dir.join("crash-4242.log"), report.as_bytes()).unwrap();
    let later = std::time::SystemTime::now() + std::time::Duration::from_secs(5);
    std::fs::File::options()
        .write(true)
        .open(dir.join("crash-4242.log"))
        .unwrap()
        .set_modified(later)
        .unwrap();
    let evidence = take_crash_evidence_in(&dir, None).expect("crash evidence must post");
    assert!(evidence.path.is_absolute());
    assert_eq!(
        evidence.path,
        dir.join("crash-4242.log.seen"),
        "the newest artifact, under its consumed (renamed) name"
    );
    assert!(
        evidence
            .sentence()
            .starts_with("aterm closed unexpectedly last time")
    );
    assert!(evidence.sentence().contains(".log.seen"));
    assert_eq!(evidence.head.len(), CRASH_HEAD_LINES);
    assert_eq!(evidence.head[0], "aterm-gui 0.1.0 crashed at unix 1.000");
    assert_eq!(evidence.head[1], "panicked at 'boom', main.rs:7");
    assert_eq!(evidence.head[2], "backtrace:");
    assert!(evidence.head[3].starts_with("0: "));
    let bytes: usize = evidence.head.iter().map(|l| l.len() + 1).sum();
    assert!(
        bytes <= usize::try_from(CRASH_HEAD_BYTES).unwrap(),
        "the head is read within the byte cap: {bytes}"
    );
    assert!(dir.join("crash-4242.log.seen").exists());
    assert!(dir.join("crash-signal-4242-7.log.seen").exists());
    assert!(!dir.join("crash-4242.log").exists());
    assert!(take_crash_evidence_in(&dir, None).is_none());
    std::fs::write(dir.join("crash-signal-1-1.log"), b"\n\n  \n").unwrap();
    let evidence = take_crash_evidence_in(&dir, None).expect("a non-empty marker posts");
    assert!(evidence.head.is_empty(), "{:?}", evidence.head);
    let _ = std::fs::remove_dir_all(&dir);
}

/// An accessibility publisher panic does not end the process, so it leaves no
/// crash evidence; its surface is the `a11y.publisher` message on the pre-App
/// inbox, asserted here.
#[cfg(a11y_tree)]
#[test]
fn an_accessibility_backend_panic_leaves_no_crash_evidence_for_the_next_launch() {
    if !crate::message_inbox::lane_zero_alone() {
        return;
    }
    let _ = crate::message_inbox::take_queued();
    let dir = std::env::temp_dir().join(format!("aterm-log-a11y-{}", std::process::id()));
    ensure_private_dir(&dir).unwrap();
    let backend = "crates/aterm-a11y/src/atspi/semantics/node.rs";
    let payload = "called `Result::unwrap()` on an `Err` value: \
                   Handshake(\"Server GUID mismatch: expected a, got b\")";
    let at_before = crate::a11y_backend::at_client_seen();
    assert_eq!(
        file_panic_report(&dir, backend, None, payload, &"panicked", &"bt"),
        None,
        "a dead accessibility publisher is not a crash of aterm"
    );
    let at_after = crate::a11y_backend::at_client_seen();
    assert!(
        take_crash_evidence_in(&dir, None).is_none(),
        "the next launch must not be told aterm closed unexpectedly"
    );
    let queued = crate::message_inbox::take_queued();
    let dead = queued
        .iter()
        .find(|m| m.message.key.as_deref() == Some(crate::message_reporters::KEY_A11Y_PUBLISHER))
        .expect("the inbox holds the a11y message");
    assert!(
        dead.message
            .detail
            .iter()
            .any(|l| l.contains("Server GUID mismatch")),
        "the message names the bus fault: {:?}",
        dead.message
    );
    if at_before {
        assert_eq!(dead.message.hold, aterm_messages::Hold::STANDING);
    } else if !at_after {
        assert_eq!(dead.message.hold, aterm_messages::Hold::LogOnly);
    }

    let path = file_panic_report(
        &dir,
        "crates/aterm-gui/src/app.rs",
        None,
        "boom",
        &"i",
        &"b",
    )
    .expect("an ordinary panic is still filed as a crash");
    assert!(path.exists());
    assert!(take_crash_evidence_in(&dir, None).is_some());
    let _ = crate::message_inbox::take_queued();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_supervisor_thread_panic_is_a_fault_record_not_a_crash() {
    let dir = std::env::temp_dir().join(format!("aterm-log-harness-{}", std::process::id()));
    ensure_private_dir(&dir).unwrap();
    let thread = "aterm-harness-s-0123456789abcdef";
    for _ in 0..2 {
        assert_eq!(
            file_panic_report(&dir, "src/run.rs", Some(thread), "boom", &"i", &"bt"),
            None
        );
    }
    assert!(
        take_crash_evidence_in(&dir, None).is_none(),
        "not a crash of aterm"
    );
    let record =
        std::fs::read_to_string(dir.join(format!("harness-fault-{}.log", std::process::id())))
            .expect("the fault record");
    assert_eq!(
        record
            .matches("supervisor fault on aterm-harness-s-")
            .count(),
        2
    );
    let path = file_panic_report(
        &dir,
        "src/run.rs",
        Some("aterm-control-listener"),
        "boom",
        &"i",
        &"bt",
    )
    .expect("another thread's panic is a crash");
    assert!(path.exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn harness_fault_records_are_capped_and_pruned() {
    let dir = Scratch::new("harness-cap");
    let backtrace = "frame\n".repeat(8 * 1024);
    let own = dir
        .0
        .path()
        .join(format!("harness-fault-{}.log", std::process::id()));
    let fault = || {
        file_panic_report(
            dir.0.path(),
            "src/run.rs",
            Some("aterm-harness-s-0123456789abcdef"),
            "boom",
            &"i",
            &backtrace,
        )
    };
    for _ in 0..2 * HARNESS_FAULT_BYTES / backtrace.len() as u64 + 2 {
        fault();
    }
    let record = read(&own);
    assert!(
        (record.len() as u64) < HARNESS_FAULT_BYTES + backtrace.len() as u64 + 1024,
        "{} bytes",
        record.len()
    );
    assert_eq!(record.matches(HARNESS_FAULT_FULL).count(), 1);
    assert!(record.ends_with(HARNESS_FAULT_FULL));

    let base = SystemTime::now() - Duration::from_secs(3600);
    let age = |path: &Path, minutes: u64| {
        File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(base + Duration::from_secs(minutes * 60))
            .unwrap();
    };
    let others = KEEP_HARNESS_FAULT_RECORDS as u64 + 4;
    for pid in 1..=others {
        let path = dir.0.path().join(format!("harness-fault-{pid}.log"));
        std::fs::write(&path, b"a fault").unwrap();
        age(&path, pid);
    }
    age(&own, others + 1);
    prune_harness_faults(dir.0.path(), std::process::id());
    let aside = format!("harness-fault-{}-", std::process::id());
    let (set_aside, mut left): (Vec<String>, Vec<String>) = std::fs::read_dir(dir.0.path())
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with("harness-fault-"))
        .partition(|name| name.starts_with(&aside));
    left.sort();
    let mut expected: Vec<String> = (others + 2 - KEEP_HARNESS_FAULT_RECORDS as u64..=others)
        .map(|pid| format!("harness-fault-{pid}.log"))
        .collect();
    expected.sort();
    assert_eq!(left, expected);
    let [set_aside] = &set_aside[..] else {
        panic!("{set_aside:?}")
    };
    assert_eq!(read(&dir.0.path().join(set_aside)), record);

    fault();
    let fresh = read(&own);
    assert_eq!(fresh.matches("supervisor fault on").count(), 1, "{fresh}");
    assert!(!fresh.contains(HARNESS_FAULT_FULL));

    prune_harness_faults(dir.0.path(), std::process::id());
    let set_aside: Vec<String> = std::fs::read_dir(dir.0.path())
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with(&aside))
        .collect();
    assert_eq!(set_aside.len(), 2, "{set_aside:?}");
    assert!(
        set_aside
            .iter()
            .any(|name| read(&dir.0.path().join(name)) == fresh),
        "{set_aside:?}"
    );
    assert!(!own.exists());
}

/// The killed run, reported once: an empty, unlocked `app` marker of another pid
/// is consumed (renamed `.seen`) and named with `aterm.log` beside it.
#[cfg(unix)]
#[test]
fn a_killed_app_run_is_reported_once_and_nothing_else_is() {
    use crate::crash_signal::{MarkerOwner, markers};
    let dir = std::env::temp_dir().join(format!("aterm-log-killed-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    ensure_private_dir(&dir).unwrap();
    let own = std::process::id();
    let gone = own.wrapping_add(1_000_000);
    let killed = dir.join(markers::file_name(gone, 5, MarkerOwner::App));
    std::fs::write(&killed, b"").unwrap();
    std::fs::write(
        dir.join(markers::file_name(gone, 6, MarkerOwner::Other)),
        b"",
    )
    .unwrap();
    std::fs::write(dir.join(markers::file_name(own, 7, MarkerOwner::App)), b"").unwrap();
    let (fd, live) =
        markers::create_locked(&dir, &markers::file_name(gone + 1, 8, MarkerOwner::App)).unwrap();
    std::fs::write(dir.join("aterm.log"), b"last line before the kill\n").unwrap();

    assert!(
        take_crash_evidence_in(&dir, None).is_none(),
        "control: the crash scan alone never saw a killed run"
    );
    let evidence = take_kill_evidence_in(&dir, own, None).expect("the killed run posts");
    assert_eq!(
        evidence.marker,
        killed.with_file_name(format!(
            "{}.seen",
            killed.file_name().unwrap().to_str().unwrap()
        ))
    );
    assert!(evidence.marker.exists() && !killed.exists());
    assert_eq!(evidence.log, dir.join("aterm.log"));
    assert!(evidence.sentence().contains("killed"));
    assert!(
        take_kill_evidence_in(&dir, own, None).is_none(),
        "consumed: it posts exactly once"
    );
    assert!(live.exists(), "a live owner's marker is not evidence");
    // SAFETY: closes the descriptor `create_locked` returned, once.
    unsafe { libc::close(fd) };
    let _ = std::fs::remove_dir_all(&dir);
}

fn queued_logger(path: &Path, cap: usize) -> &'static FileLogger {
    let file = RotatingFile::open(path.to_path_buf(), RotationBudget::ATERM_LOG).unwrap();
    let logger: &'static FileLogger = Box::leak(Box::new(FileLogger::new(file, cap)));
    logger.pending.lock().unwrap().attended = true;
    logger
}

fn logged(logger: &FileLogger, n: usize) {
    logger.write_record(aterm_log::Level::Info, "t", &format!("line {n}"));
}

fn bodies(path: &Path) -> Vec<String> {
    read(path)
        .lines()
        .map(|l| l.rsplit_once(": ").map_or(l, |(_, body)| body).to_string())
        .collect()
}

fn lines(range: std::ops::RangeInclusive<usize>) -> Vec<String> {
    range.map(|n| format!("line {n}")).collect()
}

/// Exit can hand logging back before the newly spawned writer takes over.
#[cfg(unix)]
#[test]
fn an_exit_handoff_cannot_be_reversed_by_a_late_writer() {
    let dir = Scratch::new("terminal-handoff");
    let path = dir.log();
    let logger = FileLogger::new(
        RotatingFile::open(path.clone(), RotationBudget::ATERM_LOG).unwrap(),
        LOG_QUEUE_BYTES,
    );
    logged(&logger, 0);
    assert_eq!(bodies(&path), lines(0..=0), "synchronous before takeover");
    assert!(logger.attend(), "an ordinary writer takes the queue over");
    logged(&logger, 1);
    assert_eq!(bodies(&path), lines(0..=0), "the writer's line queued");
    logger.unattend();
    assert!(logger.flush_within(aterm_spec::wait::MUST_HAPPEN));
    logged(&logger, 2);
    assert_eq!(bodies(&path), lines(0..=2), "exit drains and hands back");

    let late_path = dir.0.path().join("late.log");
    let late = FileLogger::new(
        RotatingFile::open(late_path.clone(), RotationBudget::ATERM_LOG).unwrap(),
        LOG_QUEUE_BYTES,
    );
    late.unattend();
    let _ = late.attend();
    logged(&late, 3);
    assert_eq!(
        bodies(&late_path),
        lines(3..=3),
        "EXIT_HANDOFF_MUST_NOT_REATTEND_THE_LOG_QUEUE"
    );
}

#[test]
fn a_record_returns_while_the_file_is_held_and_lands_in_order() {
    let dir = Scratch::new("held");
    let file = RotatingFile::open(dir.log(), RotationBudget::ATERM_LOG).unwrap();
    let logger: &'static FileLogger = Box::leak(Box::new(FileLogger::new(file, LOG_QUEUE_BYTES)));
    logger.start_writer().expect("the writer starts");
    let took_over = aterm_spec::wait::within(
        aterm_spec::wait::MUST_HAPPEN,
        Duration::from_millis(1),
        || logger.pending.lock().unwrap().attended.then_some(()),
    );
    assert!(took_over.is_some(), "the writer took the queue over");
    let held = logger.file.lock().unwrap();
    let (done, returned) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for n in 1..=3 {
            logged(logger, n);
        }
        let _ = done.send(());
    });
    returned
        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
        .expect("three records returned with the file held");
    assert_eq!(read(&dir.log()), "", "nothing reached the held file");
    drop(held);
    assert!(logger.flush_within(aterm_spec::wait::MUST_HAPPEN));
    assert_eq!(bodies(&dir.log()), lines(1..=3));
}

#[test]
fn a_full_queue_is_appended_in_order_by_the_thread_that_overflows_it() {
    let dir = Scratch::new("full");
    let one = format_record(
        epoch_ms_now(),
        std::process::id(),
        aterm_log::Level::Info,
        "t",
        "line 1",
    )
    .len();
    let logger = queued_logger(&dir.log(), 3 * one + 2);
    for n in 1..=3 {
        logged(logger, n);
    }
    assert_eq!(read(&dir.log()), "", "three lines fit the queue");
    logged(logger, 4);
    assert_eq!(
        bodies(&dir.log()),
        lines(1..=4),
        "the overflow lands all, in order"
    );
    logged(logger, 5);
    assert_eq!(bodies(&dir.log()), lines(1..=4), "the queue took the next");
    assert!(logger.flush_within(aterm_spec::wait::MUST_HAPPEN));
    assert_eq!(bodies(&dir.log()), lines(1..=5));
}

#[test]
fn a_flush_is_bounded_and_lands_the_queue() {
    let dir = Scratch::new("flush");
    let logger = queued_logger(&dir.log(), LOG_QUEUE_BYTES);
    logged(logger, 1);
    let held = logger.file.lock().unwrap();
    assert!(
        !logger.flush_within(Duration::from_millis(5)),
        "the file was held"
    );
    drop(held);
    assert_eq!(read(&dir.log()), "", "still queued");
    assert!(logger.flush_within(aterm_spec::wait::MUST_HAPPEN));
    assert_eq!(bodies(&dir.log()), lines(1..=1));
}

#[cfg(target_os = "macos")]
#[test]
fn an_objc_exception_report_is_on_disk_before_the_sink_returns() {
    const STAGE: &str = "ATERM_TEST_OBJC_REPORT_PATH";
    if let Some(path) = std::env::var_os(STAGE) {
        let path = Path::new(&path);
        let logger = queued_logger(path, LOG_QUEUE_BYTES);
        assert!(FILE_LOGGER.set(logger).is_ok());
        assert!(aterm_log::set_logger(logger).is_ok());
        aterm_log::set_max_level(LevelFilter::Error);
        objc_containment_sink(&aterm_objc::ContainedException {
            method: "applicationDidFinishLaunching:",
            class: "NSException",
            name: "LaunchException",
            reason: &format!("launch failed {}", "r".repeat(2048)),
            call_stack: &(0..48)
                .map(|index| format!("native-frame-{index:02} {}", "symbol".repeat(30)))
                .collect::<Vec<_>>()
                .join(" | "),
            ordinal: 1,
        });
        let report = read(path);
        for detail in [
            "applicationDidFinishLaunching:",
            "LaunchException",
            "launch failed",
            "native-frame-00",
            "native-frame-47",
        ] {
            assert!(
                report.contains(detail),
                "queued report lost {detail:?}: {report}"
            );
        }
        for index in 0..48 {
            assert!(
                report.contains(&format!("native-frame-{index:02} ")),
                "the bounded header or preceding frames discarded frame {index}"
            );
        }
        return;
    }
    let dir = Scratch::new("objc-report");
    let mut helper = crate::test_alone::StandIn::this_test();
    helper.command().env(STAGE, dir.log());
    let ran = helper.spawn().selected();
    assert_eq!(ran.status.and_then(|s| s.code()), Some(0), "{}", ran.stderr);
}

/// Landed at the end: a process that `exit`s with a line still queued
/// (its writer never ran) finds it on disk, through the `exit(3)`
/// handler; and a line logged after that handler ran (here by a handler
/// registered before it, so run after it) is appended by its own thread,
/// not queued for the writer the process is losing.
#[cfg(unix)]
#[test]
fn an_exit_lands_the_lines_still_queued() {
    const STAGE: &str = "ATERM_TEST_LOG_EXIT_PATH";
    extern "C" fn logs_after_the_flush() {
        if let Some(logger) = FILE_LOGGER.get() {
            logged(logger, 2);
        }
    }
    unsafe extern "C" {
        fn atexit(cb: extern "C" fn()) -> libc::c_int;
    }
    if let Some(path) = std::env::var_os(STAGE) {
        let logger = queued_logger(Path::new(&path), LOG_QUEUE_BYTES);
        assert!(
            FILE_LOGGER.set(logger).is_ok(),
            "this process's only logger"
        );
        // SAFETY: a plain `extern "C"` function with no captured state.
        assert_eq!(unsafe { atexit(logs_after_the_flush) }, 0, "atexit");
        flush_at_exit();
        logged(logger, 1);
        assert_eq!(read(Path::new(&path)), "", "queued at the exit");
        std::process::exit(0);
    }
    let dir = Scratch::new("exit");
    let mut helper = crate::test_alone::StandIn::this_test();
    helper.command().env(STAGE, dir.log());
    let ran = helper.spawn().selected();
    assert_eq!(ran.status.and_then(|s| s.code()), Some(0), "{}", ran.stderr);
    assert_eq!(bodies(&dir.log()), lines(1..=2));
}

#[test]
fn format_record_sanitizes_and_terminates_line() {
    let line = format_record(
        1_700_000_000_123,
        2061,
        aterm_log::Level::Warn,
        "containment_audit",
        "DENIED: image write '\x1b]0;x\n'",
    );
    assert_eq!(
        line,
        "1700000000.123 2061 WARN containment_audit: DENIED: image write '\u{fffd}]0;x\u{fffd}'\n"
    );
}

#[test]
fn format_record_keeps_more_of_a_warning() {
    let body = "x".repeat(900);
    let warn = format_record(0, 1, aterm_log::Level::Warn, "t", &body);
    assert!(warn.contains(&body), "a 900-byte warning is kept whole");
    let info = format_record(0, 1, aterm_log::Level::Info, "t", &body);
    assert!(info.trim_end().ends_with('…'));
    assert!(info.len() < 600);
}

#[test]
fn crash_evidence_keeps_only_the_newest_shown_reports() {
    let dir = Scratch::new("prune");
    let base = SystemTime::now() - Duration::from_secs(3600);
    for i in 0..12_u64 {
        let p = dir.0.path().join(format!("crash-{i}.log.seen"));
        std::fs::write(&p, b"old report").unwrap();
        File::options()
            .write(true)
            .open(&p)
            .unwrap()
            .set_modified(base + Duration::from_secs(i * 60))
            .unwrap();
    }
    let fresh = dir.0.path().join("crash-99.log");
    std::fs::write(&fresh, b"panicked").unwrap();
    File::options()
        .write(true)
        .open(&fresh)
        .unwrap()
        .set_modified(base - Duration::from_secs(60))
        .unwrap();
    let evidence = take_crash_evidence_in(dir.0.path(), None).unwrap();
    assert!(evidence.path.ends_with("crash-99.log.seen"), "{evidence:?}");
    assert!(dir.0.path().join("crash-99.log.seen").exists());
    let mut left: Vec<String> = std::fs::read_dir(dir.0.path())
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    left.sort();
    let mut expected: Vec<String> = (4..12).map(|i| format!("crash-{i}.log.seen")).collect();
    expected.push("crash-99.log.seen".into());
    expected.sort();
    assert_eq!(left, expected);
}
