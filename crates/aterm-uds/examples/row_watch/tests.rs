// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn a_row_keeps_the_order_of_its_two_streams_and_its_exit_status() {
    let mut output = Vec::new();
    let status = supervise(
        [
            "sh",
            "-c",
            "printf 'running\n' >&2; printf '%s\n' \"$1\"; exit 23",
            "row",
            "a literal $1",
        ],
        &mut output,
        |_, _| false,
    )
    .unwrap()
    .unwrap();
    assert_eq!(status.code(), Some(23));
    assert_eq!(output, b"running\na literal $1\n");
}

#[test]
fn an_observed_stall_returns_the_last_unterminated_line_and_reaps_the_child() {
    let mut output = Vec::new();
    let mut root = None;
    let status = supervise(
        [
            "sh",
            "-c",
            "printf 'test still running ... '; exec sleep 600",
        ],
        &mut output,
        |child, bytes| {
            if let Child::Running(pid) = child {
                root = Some(pid);
            }
            bytes > 0
        },
    )
    .unwrap();
    assert!(status.is_none());
    assert_eq!(output, b"test still running ... ");
    assert_eq!(
        aterm_uds::process::process_presence(i32::try_from(root.unwrap()).unwrap()),
        aterm_uds::process::ProcessPresence::Absent
    );
}

#[test]
fn a_stop_before_spawn_runs_nothing() {
    let dir = aterm_tempfile::tempdir().unwrap();
    let marker = dir.path().join("started");
    let status = supervise(
        [std::ffi::OsStr::new("touch"), marker.as_os_str()],
        &mut Vec::new(),
        |child, _| matches!(child, Child::Pending),
    )
    .unwrap();
    assert!(status.is_none());
    assert!(!marker.exists());
}

#[test]
fn stall_evidence_includes_a_helper_in_its_own_group_and_excludes_the_parent() {
    use aterm_uds::spawnfd::{Stdio, command};

    let mut helper = command("/bin/cat")
        .process_group(0)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let helper_pid = helper.id();
    let root = std::process::id();
    let report = evidence(Some(root));
    drop(helper.stdin.take());
    assert!(helper.wait().unwrap().success());
    assert!(
        report.contains(&format!("[pid={helper_pid} parent={root} ")),
        "{report}"
    );
    let parent = std::os::unix::process::parent_id();
    assert!(!report.contains(&format!("[pid={parent} ")), "{report}");
}

#[test]
fn a_thread_reports_only_its_current_syscall_number() {
    assert_eq!(syscall_number("98 0xffff 0x6 0x0"), Some(98));
    assert_eq!(syscall_number("-1 0xffff 0xfffe"), None);
    assert_eq!(syscall_number("running"), None);
}

#[test]
fn a_residual_group_member_is_reported_even_after_reparenting_or_root_exit() {
    let root = u32::MAX - 10;
    let member = |pid, ppid, pgid| aterm_sysprobe::ProcessReading {
        pid,
        ppid,
        pgid,
        ..Default::default()
    };
    let orphan = member(root + 1, 1, root);
    let peer = member(root + 2, 1, root + 2);
    let mut sweep = aterm_sysprobe::Sweep {
        procs: vec![orphan, peer],
        truncated: false,
    };
    for root_present in [false, true] {
        if root_present {
            sweep.procs.push(member(root, 1, root));
        }
        let report = evidence_from(root, &sweep);
        assert_eq!(
            report.contains("descendants=unavailable"),
            !root_present,
            "{report}"
        );
        assert!(
            report.contains(&format!("[pid={} parent=1 group={root} ", root + 1)),
            "{report}"
        );
        assert!(!report.contains(&format!("[pid={} ", root + 2)), "{report}");
    }
}
