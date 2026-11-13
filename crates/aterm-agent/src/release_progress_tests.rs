// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn survey_checks_kernel_identity_instead_of_trusting_a_recent_record() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let registry = dir.path().join(DIRECTORY);
    std::fs::create_dir(&registry).unwrap();
    let pid = std::process::id();
    let (boot, start) = identity(pid).unwrap();
    let path = registry.join("0.156.0.json");
    let record = |start| aterm_json::json!({"holder": {"pid": pid, "boot": boot, "start": start}, "rows": ["red"]});
    std::fs::write(&path, record(start).to_string()).unwrap();
    assert!(survey(dir.path())[0].starts_with("release_line alive=1 "));
    // Negative control: the same pid and a recent row cannot revive another start.
    std::fs::write(&path, record(start.wrapping_add(1)).to_string()).unwrap();
    assert!(survey(dir.path())[0].starts_with("release_line alive=0 "));
}

/// Each kind of positive kernel evidence reads dead, and only an exact identity reads alive.
/// NEGATIVE CONTROLS: a malformed pid (0, launchd's or init's 1, or one past `i32::MAX`) is
/// unknown, never dead, where the previous `kill(pid, 0)` rule called 0 and `u32::MAX` dead
/// with no kernel read at all.
#[test]
fn judge_needs_positive_kernel_evidence_for_death() {
    let pid = std::process::id();
    let (boot, start) = identity(pid).unwrap();
    assert_eq!(judge(pid, &boot, start), Judged::Alive);
    assert_eq!(judge(pid, &boot, start.wrapping_add(1)), Judged::Dead);
    let other_boot = if boot.starts_with('0') { "1" } else { "0" };
    assert_eq!(judge(pid, other_boot, start), Judged::Dead);
    // A reaped child: its pid is absent, or names a later process with another start.
    let mut child = aterm_uds::spawnfd::command("/bin/sleep")
        .arg("60")
        .spawn()
        .unwrap();
    let child_pid = child.id();
    let child_start = identity(child_pid).map(|(_, start)| start);
    child.kill().unwrap();
    child.wait().unwrap();
    let child_start = child_start.unwrap();
    assert_eq!(judge(child_pid, &boot, child_start), Judged::Dead);
    for malformed in [0, 1, u32::MAX] {
        assert_eq!(
            judge(malformed, &boot, start),
            Judged::Unknown,
            "{malformed}"
        );
    }
}

/// A kernel read that fails or is refused is unknown, never death, for a pid that passes the
/// pid filter: an unreadable boot id, an unknown presence, and a present process whose start
/// cannot be read (another user's, or a zombie).
/// NEGATIVE CONTROLS: the same reads answering prove death (another boot, an absent pid, a
/// different start) and life (the same start).
#[test]
fn a_failed_kernel_read_is_unknown_and_never_death() {
    use aterm_uds::process::ProcessPresence;
    let judged = |boot: Option<&str>, presence: ProcessPresence, start: Option<u64>| {
        judge_with(
            4242,
            "b",
            7,
            &Kernel {
                boot_id: &|| boot.map(str::to_owned),
                presence: &|pid| {
                    assert_eq!(pid, 4242);
                    presence
                },
                start_of: &|_| start,
            },
        )
    };
    assert_eq!(judged(None, ProcessPresence::Absent, None), Judged::Unknown);
    assert_eq!(
        judged(Some("b"), ProcessPresence::Unknown, Some(8)),
        Judged::Unknown
    );
    assert_eq!(
        judged(Some("b"), ProcessPresence::Present, None),
        Judged::Unknown
    );
    assert_eq!(
        judged(Some("c"), ProcessPresence::Present, Some(7)),
        Judged::Dead
    );
    assert_eq!(
        judged(Some("b"), ProcessPresence::Absent, None),
        Judged::Dead
    );
    assert_eq!(
        judged(Some("b"), ProcessPresence::Exited, None),
        Judged::Dead
    );
    assert_eq!(
        judged(Some("b"), ProcessPresence::Present, Some(8)),
        Judged::Dead
    );
    assert_eq!(
        judged(Some("b"), ProcessPresence::Present, Some(7)),
        Judged::Alive
    );
}

/// The row answering a holder query is this kernel's judgement of exactly that identity.
#[test]
fn a_holder_row_judges_exactly_the_identity_asked() {
    use aterm_ctl::release_holder::Identity;
    let pid = std::process::id();
    let (boot, start) = identity(pid).unwrap();
    let me = Identity { pid, boot, start };
    assert_eq!(me.read(&holder_row(&me)), Some(Some(true)));
    let reused = Identity {
        start: start.wrapping_add(1),
        ..me.clone()
    };
    assert_eq!(reused.read(&holder_row(&reused)), Some(Some(false)));
    // NEGATIVE CONTROL: the row about one identity says nothing of another.
    assert_eq!(me.read(&holder_row(&reused)), None);
}

fn current_record(padding: usize) -> String {
    let pid = std::process::id();
    let (boot, start) = identity(pid).unwrap();
    aterm_json::json!({"holder":{"pid":pid,"boot":boot,"start":start},
        "rows":[],"padding":"x".repeat(padding)})
    .to_string()
}

#[test]
fn survey_omits_oversize_valid_json_and_growth_after_admission() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let registry = dir.path().join(DIRECTORY);
    std::fs::create_dir(&registry).unwrap();
    let path = registry.join("line.json");
    let small = current_record(0);
    std::fs::write(&path, &small).unwrap();
    assert_eq!(survey(dir.path()).len(), 1);
    let (held, metadata) = aterm_uds::regular_file::open(&path).unwrap();
    let large = current_record(MAX_RECORD_BYTES as usize);
    assert!(aterm_json::from_str::<aterm_json::Value>(&large).is_ok());
    std::fs::write(&path, large).unwrap();
    assert!(read_record(held, metadata.len()).is_none());
    assert!(survey(dir.path()).is_empty());
    std::fs::write(&path, small).unwrap();
    assert_eq!(survey(dir.path()).len(), 1);
}

#[test]
fn admitted_record_does_not_reopen_a_replaced_path() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let registry = dir.path().join(DIRECTORY);
    std::fs::create_dir(&registry).unwrap();
    let path = registry.join("line.json");
    let old = current_record(0);
    std::fs::write(&path, &old).unwrap();
    let (held, metadata) = aterm_uds::regular_file::open(&path).unwrap();
    let replacement = dir.path().join("replacement");
    std::fs::write(&replacement, current_record(MAX_RECORD_BYTES as usize)).unwrap();
    std::fs::rename(&replacement, &path).unwrap();
    assert_eq!(read_record(held, metadata.len()).unwrap(), old.as_bytes());
    assert!(survey(dir.path()).is_empty());
}

#[test]
fn survey_refuses_final_aliases_directories_and_fifos() {
    use std::os::unix::ffi::OsStrExt as _;
    if !aterm_uds::test_binary::isolated_lifecycle(
        "release_progress::tests::survey_refuses_final_aliases_directories_and_fifos",
    ) {
        return;
    }
    let dir = aterm_tempfile::tempdir().unwrap();
    let registry = dir.path().join(DIRECTORY);
    std::fs::create_dir(&registry).unwrap();
    let target = dir.path().join("target");
    std::fs::write(&target, current_record(0)).unwrap();
    std::os::unix::fs::symlink(&target, registry.join("alias.json")).unwrap();
    std::fs::create_dir(registry.join("directory.json")).unwrap();
    let fifo = registry.join("fifo.json");
    let c_fifo = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
    // SAFETY: the C string is terminated and live; this creates only a FIFO
    // in the test's owned directory, with no reader or writer to wait for.
    assert_eq!(unsafe { libc::mkfifo(c_fifo.as_ptr(), 0o600) }, 0);
    assert!(survey(dir.path()).is_empty());
    std::fs::write(registry.join("valid.json"), current_record(0)).unwrap();
    assert_eq!(survey(dir.path()).len(), 1);
}
