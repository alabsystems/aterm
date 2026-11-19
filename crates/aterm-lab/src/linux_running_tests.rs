// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::cell::Cell;

fn vm() -> VmState {
    VmState {
        status: "Running".into(),
        dir: "/owned/lima/aterm-linux".into(),
        mounts: vec![("/owned/source".into(), false)],
        cpus: 8,
        memory: 16 << 30,
        disk: 60 << 30,
    }
}

fn boot_record() -> Booter {
    Booter {
        pid: 2,
        birth: Some((1, 0)),
        since: 1,
        verb: "lab linux up".into(),
        checkout: "/owned/source".into(),
        kept: true,
        vm_pid: Some(42),
        vm_birth: Some((2, 3)),
    }
}

fn output(code: Option<i32>, signal: Option<i32>) -> Out {
    Out {
        code,
        signal,
        text: "actual command output\n".into(),
        secs: 1.0,
        not_started: None,
        lost: None,
    }
}

#[test]
fn missing_stopped_stale_and_mismatched_boots_never_run_a_command() {
    let vm = vm();
    let mut stopped = vm.clone();
    stopped.status = "Stopped".into();
    let mut writable = vm.clone();
    writable.mounts[0].1 = true;
    let mut other_mount = vm.clone();
    other_mount.mounts[0].0 = "/other/source".into();
    let boot = boot_record();
    let mut dead = boot_record();
    dead.kept = false;
    let mut foreign = boot_record();
    foreign.checkout = "/other/source".into();
    let mut unrecorded = boot_record();
    unrecorded.vm_birth = None;
    let mounts = vec!["/owned/source".into()];
    let calls = Cell::new(0);
    for (name, vm, boot, agent) in [
        ("missing VM", None, Some(&boot), Some((42, (2, 3)))),
        (
            "stopped VM",
            Some(&stopped),
            Some(&boot),
            Some((42, (2, 3))),
        ),
        (
            "writable mount",
            Some(&writable),
            Some(&boot),
            Some((42, (2, 3))),
        ),
        (
            "other mount",
            Some(&other_mount),
            Some(&boot),
            Some((42, (2, 3))),
        ),
        ("missing boot", Some(&vm), None, Some((42, (2, 3)))),
        ("dead one-shot", Some(&vm), Some(&dead), Some((42, (2, 3)))),
        (
            "other checkout",
            Some(&vm),
            Some(&foreign),
            Some((42, (2, 3))),
        ),
        (
            "unrecorded birth",
            Some(&vm),
            Some(&unrecorded),
            Some((42, (2, 3))),
        ),
        ("unread agent", Some(&vm), Some(&boot), None),
        ("reused PID", Some(&vm), Some(&boot), Some((42, (9, 9)))),
        ("other agent", Some(&vm), Some(&boot), Some((43, (2, 3)))),
    ] {
        let result = in_running(
            Facts {
                vm,
                boot,
                agent,
                booter_alive: false,
            },
            "/owned/source",
            &mounts,
            || {
                calls.set(calls.get() + 1);
                output(Some(0), None)
            },
        );
        assert!(result.is_err(), "{name}");
    }
    assert_eq!(
        calls.get(),
        0,
        "refusals never enter the command transport or an initialization path"
    );
}

#[test]
fn a_kept_original_boot_or_live_one_shot_runs_once_and_preserves_actual_status() {
    let vm = vm();
    let mounts = vec!["/owned/source".into()];
    for kept in [true, false] {
        let mut boot = boot_record();
        boot.kept = kept;
        for (code, signal, expected) in [(Some(37), None, 37), (None, Some(9), 137)] {
            let calls = Cell::new(0);
            let out = in_running(
                Facts {
                    vm: Some(&vm),
                    boot: Some(&boot),
                    agent: Some((42, (2, 3))),
                    booter_alive: !kept,
                },
                "/owned/source",
                &mounts,
                || {
                    calls.set(calls.get() + 1);
                    output(code, signal)
                },
            )
            .unwrap();
            assert_eq!(calls.get(), 1);
            assert_eq!(out.text, "actual command output\n");
            assert_eq!(out.code, code);
            assert_eq!(out.signal, signal);
            assert_eq!(exec_exit(&out, &mut |_| {}).unwrap(), expected);
        }
    }
}

#[test]
fn literal_command_arguments_accept_an_optional_separator() {
    assert!(arguments(&[]).is_err());
    let command = vec!["--".into(), "/bin/sh".into(), "-c".into(), "exit 37".into()];
    assert_eq!(arguments(&command).unwrap(), &command[1..]);
    let literal = vec!["printenv".into(), "HOME".into()];
    assert_eq!(arguments(&literal).unwrap(), literal);
}
