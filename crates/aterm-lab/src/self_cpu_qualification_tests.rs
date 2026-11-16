// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

pub(crate) fn proof(worker_reported: u64) -> Proof {
    let before = Cut {
        pid: 1,
        start: 2,
        version: 3,
        begin_ns: 1,
        end_ns: 2,
        process_before: Counter {
            ns: 100_000,
            precision_ns: 2,
        },
        process_after: Counter {
            ns: 100_001,
            precision_ns: 2,
        },
        threads: vec![Thread {
            id: 7,
            name: Some(WORKER.into()),
            cpu: Some(Counter {
                ns: 1000,
                precision_ns: 2000,
            }),
        }],
        listed: 1,
        inspected: 1,
        unread: 0,
        capped: false,
    };
    let mut after = before.clone();
    after.begin_ns = WORK_NS + 3;
    after.end_ns = WORK_NS + 4;
    // A delayed live process counter is deliberately far below the worker's published CPU.
    after.process_before.ns = 101_000;
    after.process_after.ns = 101_001;
    after.threads[0].cpu.as_mut().unwrap().ns = worker_reported;
    let child = ChildProof {
        schema: 2,
        kernel_vm: true,
        worker: WORKER.into(),
        work_wall_ns: WORK_NS,
        work_iterations: 1,
        before,
        after,
    };
    let mut proof = Proof {
        schema: 2,
        kernel_vm: true,
        terminal: Some(TerminalOracle {
            source: TERMINAL_SOURCE.into(),
            pid: 1,
            exit_code: Some(0),
            user_ns: 9_998_000,
            system_ns: 0,
            timebase_numer: 125,
            timebase_denom: 3,
            rounding_upper_ns: 2000,
        }),
        child_output_complete: true,
        child_stdout: aterm_json::to_vec(&child).unwrap(),
        child_stderr: Vec::new(),
        bound: None,
        qualification_error: None,
    };
    match exit_accounting(&proof) {
        Ok(bound) => proof.bound = Some(bound),
        Err(why) => proof.qualification_error = Some(why),
    }
    proof
}

fn edit_child(proof: &mut Proof, change: impl FnOnce(&mut ChildProof)) {
    let mut child: ChildProof = aterm_json::from_slice(&proof.child_stdout).unwrap();
    change(&mut child);
    proof.child_stdout = aterm_json::to_vec(&child).unwrap();
}

#[test]
fn eighty_percent_uses_absolute_worker_and_saved_exit_accounting() {
    let bound = validate(&proof(8_000_000)).unwrap();
    assert_eq!(
        (
            bound.worker_published_ns,
            bound.exit_accounted_ns,
            bound.exit_accounted_upper_ns
        ),
        (8_000_000, 9_998_000, 10_000_000)
    );
    assert!(validate(&proof(7_999_999)).is_err());
    assert!(validate(&proof(10_000_001)).is_err());
}

#[test]
fn changed_missing_or_ambiguous_worker_cannot_pass() {
    for change in 0..4 {
        let mut p = proof(9_000_000);
        edit_child(&mut p, |c| match change {
            0 => c.after.threads[0].id += 1,
            1 => c.after.threads[0].name = Some("other".into()),
            2 => c.after.threads[0].cpu = None,
            _ => {
                let mut other = c.after.threads[0].clone();
                other.id += 1;
                c.after.threads.push(other);
                c.after.listed += 1;
                c.after.inspected += 1;
            }
        });
        assert!(validate(&p).is_err());
    }
}

#[test]
fn only_own_successful_qualified_terminal_accounting_can_pass() {
    for change in 0..10 {
        let mut p = proof(9_000_000);
        if change == 0 {
            p.terminal = None;
        } else {
            let t = p.terminal.as_mut().unwrap();
            match change {
                1 => t.pid += 1,
                2 => t.exit_code = Some(1),
                3 => t.source = "live_process_read".into(),
                4 => t.timebase_numer = 7,
                5 => t.rounding_upper_ns = 0,
                6 => {
                    t.user_ns = u64::MAX - u64::MAX % 1000;
                    t.system_ns = 1000;
                }
                7 => t.user_ns = 0,
                8 => t.user_ns += 1,
                _ => t.user_ns = u64::MAX - u64::MAX % 1000,
            }
        }
        assert!(validate(&p).is_err());
    }
    assert_eq!(terminal_rounding(125, 3), Some(2000));
    assert_eq!(terminal_rounding(1, 1), Some(2000));
    assert_eq!(terminal_rounding(0, 1), None);
    assert_eq!(terminal_rounding(1, 0), None);
    assert_eq!(terminal_rounding(3, 1), None);
}

#[test]
fn complete_protocol_and_declared_evidence_are_required() {
    assert_eq!(require_guest(Some(false)), Err("physical_host_refused"));
    assert_eq!(require_guest(None), Err("kernel_vm_evidence_unavailable"));
    for change in 0..12 {
        let mut p = proof(9_000_000);
        match change {
            0 => p.kernel_vm = false,
            1 => p.child_output_complete = false,
            2 => p.child_stdout.clear(),
            3 => p.bound = None,
            4 => p.bound.as_mut().unwrap().worker_published_ns += 1,
            5 => p.qualification_error = Some("incomplete".into()),
            6 => edit_child(&mut p, |c| c.work_wall_ns -= 1),
            7 => edit_child(&mut p, |c| c.kernel_vm = false),
            8 => edit_child(&mut p, |c| c.work_wall_ns += 2),
            9 => edit_child(&mut p, |c| c.work_iterations = 0),
            10 => p.schema = 1,
            _ => edit_child(&mut p, |c| c.schema = 1),
        }
        assert!(validate(&p).is_err());
    }
    let mut padded = proof(9_000_000);
    padded
        .child_stdout
        .resize(MAX_CHILD_OUTPUT as usize + 1, b' ');
    assert!(validate(&padded).is_err());
}
