// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_uds::spawnfd;
use std::time::{Duration, Instant};

#[test]
fn an_actual_legacy_wait_cannot_be_relabelled_as_measured_zero() {
    let mut child = spawnfd::command("/usr/bin/true").spawn().unwrap();
    assert!(child.wait().unwrap().success());
    assert!(child.is_reaped());
    // The actual original wait retained status but no accounting. A late
    // measurement request cannot fabricate zero counters from that cache.
    assert!(try_reap(&mut child, true).is_err());
}

#[test]
fn original_owning_wait_preserves_success_failure_and_cancel_accounting() {
    use std::os::unix::process::ExitStatusExt as _;
    for (script, cancel, expected) in [
        ("exit 0", false, Some(0)),
        ("exit 7", false, Some(7)),
        ("exec /bin/sleep 60", true, None),
    ] {
        let mut command = spawnfd::command("/bin/sh");
        command.arg("-c").arg(script);
        let mut child = command.spawn().unwrap();
        if cancel {
            child.kill().unwrap();
        }
        let deadline = Instant::now() + Duration::from_secs(60);
        let (status, observed) = loop {
            if let Some(result) = try_reap(&mut child, true).unwrap() {
                break result;
            }
            assert!(
                Instant::now() < deadline,
                "original owning wait did not complete"
            );
            std::thread::sleep(Duration::from_millis(5));
        };
        assert_eq!(status.code(), expected);
        if cancel {
            assert_eq!(status.signal(), Some(9));
        }
        // This reads the same child's cached owning-wait result. It does
        // not perform a second native wait or select a process by PID.
        let original = child.try_wait_with_rusage().unwrap().unwrap();
        assert_eq!(observed, observation(original));
        assert!(observed.cpu_times.is_some());
        assert!(observed.peak_rss_bytes.is_some());
        assert_eq!(observed.scope, AccountingScope::ChildAndWaitedDescendantsV1);
    }
}

#[test]
fn an_original_wait_preserves_tab_origin_and_final_split_without_a_monitor_sweep() {
    for accounted in [false, true] {
        let binding = aterm_types::resource_admission::Binding {
            session: "tacde112200".into(),
            attempt: [71; 16],
        };
        // The waited descendant spins long enough to show in Linux's descendant time, which
        // `/proc/<pid>/stat` counts in whole clock ticks (10 ms at 100 Hz): a `/usr/bin/true`
        // reads as none.
        let mut child = spawnfd::command("/bin/sh")
            .args([
                "-c",
                "/bin/sh -c 'i=0; while [ $i -lt 30000 ]; do i=$((i+1)); done'; /usr/bin/true",
            ])
            .spawn()
            .unwrap();
        crate::resource_observation::born(&binding, child.id());
        let before = crate::resource_observation::native_observation_guard().next_cursor();
        let deadline = Instant::now() + Duration::from_secs(60);
        while !child.exited().unwrap() {
            assert!(Instant::now() < deadline, "original child never exited");
            std::thread::sleep(Duration::from_millis(5));
        }
        let before_wait = aterm_uds::process::continuous_ns().unwrap();
        let (_, observed) = try_reap(&mut child, accounted).unwrap().unwrap();
        let after_wait = aterm_uds::process::continuous_ns().unwrap();
        let ledger = crate::resource_observation::native_observation_guard();
        let terminal = ledger
            .terminals_since(before)
            .find(|t| t.process.pid == child.id())
            .unwrap();
        assert_eq!(terminal.session, binding.session);
        assert!(
            terminal
                .exit_cut_ns
                .is_some_and(|cut| cut >= before_wait && cut <= after_wait),
            "the exit proof must carry the original owning wait's source cut"
        );
        assert!(
            terminal.cpu_ns.is_some(),
            "the original unreaped child's own time was not read"
        );
        assert!(
            terminal.child_cpu_ns.is_some_and(|ns| ns > 0),
            "the original child's waited descendant was not read"
        );
        assert_eq!(
            terminal.wait_cpu_ns,
            observed.cpu_times.map(|cpu| cpu.user_ns + cpu.system_ns)
        );
        assert!(terminal.wait_cpu_ns.unwrap() > 0);
    }
}
