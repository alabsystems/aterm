// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_capacity::{Fixed, Ledger, Need, Polled, Reading, Waiter, cohort_namespace::Namespace};
use std::{io::Write as _, os::unix::fs::DirBuilderExt as _, time::Duration};

const HANG: Duration = Duration::from_secs(60);

struct Running(State);
impl Running {
    fn child(&mut self) -> &mut spawnfd::Child {
        match &mut self.0 {
            State::Bound(bound) => bound.child.child_mut(),
            State::Unbound(Some(spawned)) => spawned.child_mut(),
            _ => panic!("archive must retain its original child"),
        }
    }
}
impl Drop for Running {
    fn drop(&mut self) {
        let child = match &mut self.0 {
            State::Bound(bound) => bound.child.child_mut(),
            State::Unbound(Some(spawned)) => spawned.child_mut(),
            _ => return,
        };
        if !child.is_reaped() {
            let _ = child.kill();
            let _ = child.wait_exited();
            let _ = crate::resource_accounting::try_reap(child, true);
        }
    }
}

#[test]
fn a_bound_archive_launch_keeps_its_session_and_final_receipt_without_a_gui_sweep() {
    if !aterm_uds::test_binary::isolated_lifecycle(
        "actor::owner::bootstrap_archive::execution::tests::a_bound_archive_launch_keeps_its_session_and_final_receipt_without_a_gui_sweep",
    ) {
        return;
    }
    observe_launch(false);
}

#[test]
fn an_archive_bind_failure_keeps_its_actual_child_session_and_final_receipt() {
    if !aterm_uds::test_binary::isolated_lifecycle(
        "actor::owner::bootstrap_archive::execution::tests::an_archive_bind_failure_keeps_its_actual_child_session_and_final_receipt",
    ) {
        return;
    }
    observe_launch(true);
}

fn observe_launch(unbound: bool) {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let home = scratch.path().join("home");
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&home)
        .unwrap();
    let namespace = Namespace::create_private(scratch.path(), &home).unwrap();
    let ledger = Ledger::at(namespace.fence());
    let machine = Fixed::new(Reading::of_gib(64, 60, 2));
    let claim = || match Waiter::enter_cohort(
        &ledger,
        &machine,
        Need::new("archive observation").mem(1024),
    )
    .unwrap()
    .poll(&machine)
    {
        Polled::Admitted(claim) => claim,
        other => panic!("private archive claim was not admitted: {other:?}"),
    };
    let mut original = claim();
    let binding = Binding {
        session: aterm_types::tab_id::TabId::from_bits(73).to_string(),
        attempt: [73; 16],
    };
    let mut command = spawnfd::command("/bin/sh");
    command
        .args([
            "-c",
            "printf 'archive-ledger-ready\\n'; read go; /usr/bin/true",
        ])
        .stdin(spawnfd::Stdio::piped())
        .stdout(spawnfd::Stdio::piped())
        .stderr(spawnfd::Stdio::null());
    let launched = if unbound {
        let mut spawned = command
            .spawn_cohort(original.arm_cohort().unwrap())
            .unwrap();
        // A genuine child exists, but another claim cannot durably bind its birth.
        let error = claim().bind_cohort(&mut spawned).unwrap_err();
        Err(LaunchError::Unbound {
            spawned: Box::new(spawned),
            error,
        })
    } else {
        original.launch_cohort(&mut command)
    };
    let before = crate::resource_observation::native_observation_guard().next_cursor();
    let mut state = State::Unused;
    let accepted = state.accept_launch(&binding, launched);
    let mut running = Running(state);
    assert_eq!(accepted.is_err(), unbound);
    if let State::Unbound(Some(spawned)) = &mut running.0 {
        // Recover only the SAME original ARM; its earlier failed bind released nothing.
        original.bind_cohort(spawned).unwrap();
    }
    let pid = running.child().id();
    aterm_uds::deadline::prints(running.child(), "archive-ledger-ready", HANG);
    let start = aterm_sysprobe::procs::process_usage(pid)
        .unwrap()
        .start
        .unwrap();
    let observed = crate::resource_observation::snapshot(before).unwrap();
    let active = observed
        .originals
        .iter()
        .find(|entry| entry.process.pid == pid)
        .unwrap();
    assert_eq!(active.session, binding.session);
    let process = active.process;
    assert_eq!(
        process,
        crate::resource_observation::ProcessKey { pid, start }
    );
    let mut input = running.child().stdin.take().unwrap();
    input.write_all(b"go\n").unwrap();
    drop(input);
    let deadline = Instant::now() + HANG;
    let accounting = loop {
        if let Some((status, accounting)) =
            crate::resource_accounting::try_reap(running.child(), true).unwrap()
        {
            assert!(status.success());
            break accounting;
        }
        assert!(
            Instant::now() < deadline,
            "original archive child never exited"
        );
        std::thread::sleep(Duration::from_millis(5));
    };
    original.finish_cohort().unwrap();
    let observed = crate::resource_observation::snapshot(before).unwrap();
    assert!(
        !observed
            .originals
            .iter()
            .any(|entry| entry.process.pid == pid)
    );
    let finals: Vec<_> = observed
        .terminals
        .iter()
        .filter(|entry| entry.process.pid == pid)
        .collect();
    assert_eq!(finals.len(), 1);
    let terminal = finals[0];
    assert_eq!(terminal.session, binding.session);
    assert_eq!(
        terminal.process, process,
        "the receipt keeps the original generation after reaping"
    );
    assert!(terminal.cpu_ns.is_some());
    assert!(terminal.child_cpu_ns.is_some());
    assert!(terminal.exit_cut_ns.is_some());
    assert_eq!(
        terminal.wait_cpu_ns,
        accounting.cpu_times.map(|cpu| cpu.user_ns + cpu.system_ns)
    );
    crate::resource_accounting::try_reap(running.child(), true).unwrap();
    assert_eq!(
        crate::resource_observation::snapshot(before)
            .unwrap()
            .next_seq,
        observed.next_seq,
        "the cached owning wait cannot publish another receipt"
    );
}
