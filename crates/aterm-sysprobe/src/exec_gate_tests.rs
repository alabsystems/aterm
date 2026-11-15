// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::time::Duration;

use aterm_spec::wait::MUST_HAPPEN;

use super::{ExecGate, StillReading};

/// A bound a read that never ends outlasts: the refusal under test, not a latency budget.
const HELD: Duration = Duration::from_millis(20);

/// With no read in the kernel the gate closes at once, starts no read while it is closed, and
/// starts them again once the exec that closed it failed.
#[test]
fn a_gate_with_no_read_closes_and_starts_none_until_it_opens() {
    static GATE: ExecGate = ExecGate::new();
    let closed = GATE
        .close(Duration::ZERO)
        .expect("no read is in the kernel");
    assert!(GATE.reading().is_none(), "a closed gate starts no read");
    drop(closed);
    let read = GATE.reading().expect("the gate is open again");
    assert_eq!(GATE.in_kernel(), 1);
    drop(read);
    assert_eq!(GATE.in_kernel(), 0);
}

/// A read still in the kernel refuses the exec, says how many, and leaves the gate open: the
/// process goes on reading and runs its program as a child.
/// NEGATIVE CONTROL (by hand: `close` returning `Ok(Closed(self))` without its wait): the first
/// assertion gets a closed gate.
#[test]
fn a_read_in_the_kernel_refuses_the_exec_and_leaves_the_gate_open() {
    static GATE: ExecGate = ExecGate::new();
    let first = GATE.reading().expect("an open gate");
    let second = GATE.reading().expect("an open gate");
    assert_eq!(GATE.close(HELD).err(), Some(StillReading(2)));
    let third = GATE.reading().expect("a refused exec leaves the gate open");
    drop((first, second, third));
    drop(GATE.close(Duration::ZERO).expect("every read ended"));
}

/// Execs that close the gate nest (the front's, then `exec_or_run`'s under it): it stays closed
/// until the last of them has failed.
#[test]
fn a_gate_closed_twice_opens_once_both_execs_failed() {
    static GATE: ExecGate = ExecGate::new();
    let outer = GATE
        .close(Duration::ZERO)
        .expect("no read is in the kernel");
    let inner = GATE
        .close(Duration::ZERO)
        .expect("a closed gate has no read");
    drop(inner);
    assert!(GATE.reading().is_none(), "the outer exec has it closed");
    drop(outer);
    assert!(GATE.reading().is_some(), "both execs failed");
}

/// A read that leaves the kernel within the bound lets the exec go ahead, at that read's end and
/// not at the bound's, and a read asked for while the exec waits is not started. The closer's
/// bound is ten hang detectors, so a close that the read's end did not wake fails this test by
/// name instead of passing a minute late.
#[test]
fn a_read_that_ends_within_the_bound_lets_the_exec_close_the_gate() {
    static GATE: ExecGate = ExecGate::new();
    let read = GATE.reading().expect("an open gate");
    let closer = std::thread::spawn(|| GATE.close(MUST_HAPPEN * 10).map(drop));
    aterm_spec::wait::until_within(MUST_HAPPEN, "the exec closed the gate to new reads", || {
        match GATE.reading() {
            Some(late) => {
                drop(late);
                None
            }
            None => Some(()),
        }
    });
    drop(read);
    aterm_spec::wait::until_within(MUST_HAPPEN, "the read's end let the exec through", || {
        closer.is_finished().then_some(())
    });
    assert_eq!(closer.join().expect("the closer"), Ok(()));
    assert!(GATE.reading().is_some(), "the failed exec's gate is open");
}

/// The variable that makes [`real_reads_stand_in`] run.
#[cfg(target_os = "macos")]
const REAL_READS: &str = "ATERM_SYSPROBE_EXEC_GATE_REAL_READS";

/// [`real_reads_stand_in`], as the test binary names it.
#[cfg(target_os = "macos")]
const STAND_IN: &str = "exec_gate::tests::real_reads_stand_in";

/// This test binary in a process of its own (closing the process's gate in the shared runner
/// would refuse the other tests' reads): what each `kern.proc` reader of this crate answers
/// about this process with the process's gate closed, then open.
#[cfg(target_os = "macos")]
#[test]
#[ignore = "the child the law below starts; alone it returns"]
fn real_reads_stand_in() {
    if std::env::var_os(REAL_READS).is_none() {
        return;
    }
    let me = std::process::id();
    let native = std::ffi::c_int::try_from(me).expect("a pid");
    let closed = super::close(Duration::ZERO).expect("no read is in the kernel");
    println!(
        "\nclosed: record={:?} table={} job={} stalled={:?}",
        crate::kproc::record_read(me).map_err(|e| e.kind()),
        crate::kproc::records().is_some(),
        crate::jobs::macos_kinfo(native).is_some(),
        crate::lane::stalls()
            .iter()
            .map(|s| s.lane)
            .collect::<Vec<_>>(),
    );
    drop(closed);
    let listed = aterm_spec::wait::until_within(MUST_HAPPEN, "the table answered", || {
        crate::kproc::records()
    });
    println!(
        "open: record={:?} table={} job={:?}",
        crate::kproc::record_read(me).map(|r| r.map(|r| r.pid == me)),
        listed.iter().any(|r| r.pid == me),
        crate::jobs::macos_kinfo(native).map(|k| k.pid == me),
    );
}

/// EVERY `kern.proc` READER OF THIS CRATE READS INSIDE THE PROCESS'S GATE: with it closed the
/// one-process read is refused as busy, the whole-table read (its lane's worker) and the job
/// probe's read answer nothing, and no lane is named stalled (nothing is in the kernel); once
/// the exec that closed it failed, each finds this process.
/// NEGATIVE CONTROL (by hand: `jobs::macos::kinfo` without its `reading()`): `job=true` while the
/// gate is closed. (By hand: the table lane's reader inside a gate other than the process's, a
/// fresh `ExecGate`): `table=true` while the gate is closed.
#[cfg(target_os = "macos")]
#[test]
fn the_process_table_readers_read_nothing_behind_a_closed_gate() {
    let mut cmd = aterm_uds::spawnfd::command(std::env::current_exe().expect("the test binary"));
    cmd.args([
        "--exact",
        STAND_IN,
        "--ignored",
        "--nocapture",
        "--test-threads=1",
    ])
    .env(REAL_READS, "1");
    let out = aterm_uds::deadline::output_within(&mut cmd, MUST_HAPPEN * 2)
        .unwrap_or_else(|e| panic!("hang detector: the stand-in: {e:?}"));
    let text = String::from_utf8_lossy(&out.stdout);
    let said = |line: &str| text.lines().any(|l| l == line);
    assert!(
        out.status.success()
            && said("closed: record=Err(ResourceBusy) table=false job=false stalled=[]")
            && said("open: record=Ok(Some(true)) table=true job=Some(true)"),
        "{}: {text}\n{}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
}
