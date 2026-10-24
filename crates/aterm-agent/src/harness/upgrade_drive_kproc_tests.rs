// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Process table (`ps`) integration tests on the live machine.

use std::collections::BTreeMap;

use aterm_uds::spawnfd::{Child, Stdio};

use super::*;

const HANG: Duration = Duration::from_secs(60);

fn login_shell() -> Child {
    let child = Command::new("/bin/bash")
        .arg0("-kproc-fixture")
        .args([
            "--noprofile",
            "--norc",
            "-c",
            "read x",
            "na\u{ef}ve\nl",
            "t\tx",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .expect("spawn bash");
    let start = Instant::now();
    while !table()
        .iter()
        .any(|(p, _, n)| *p == child.id() && n == "kproc-fixture")
    {
        assert!(start.elapsed() < HANG, "bash's exec: not within {HANG:?}");
        std::thread::yield_now();
    }
    child
}

fn end(mut c: Child) {
    drop(c.stdin.take());
    let _ = c.kill();
    let _ = c.wait();
}

fn keyed<R: Clone>(rows: &[R], pid: impl Fn(&R) -> u32) -> BTreeMap<u32, R> {
    rows.iter().map(|r| (pid(r), r.clone())).collect()
}

/// The rows of the processes held still across the reads are the ones `ps` prints: the fixture
/// shell's and this process's whole (both name paths of this user's, whose arguments are read),
/// and launchd's ids (another user's). Only held processes are compared: a live one mid-exec or
/// mid-exit reads as its unread `(comm)` in `ps` between two kernel reads that agree.
#[test]
fn the_table_is_the_one_ps_prints() {
    let shell = login_shell();
    let (fixture, me) = (shell.id(), std::process::id());
    let held = [fixture, me, 1];
    // Each held row is named at its read, while the fixture lives.
    let named = || {
        let t = table();
        for (pid, _, name) in &t {
            if held.contains(pid) {
                name.as_str();
            }
        }
        keyed(&t, |r| r.0)
    };
    let a = named();
    let oracle = keyed(&ps_table(), |r| r.0);
    let b = named();
    end(shell);
    for pid in held {
        let (Some(x), Some(y), Some(row)) = (a.get(&pid), b.get(&pid), oracle.get(&pid)) else {
            panic!("{pid}: in both reads and in ps's table");
        };
        assert_eq!(x, y, "{pid} is held still");
        if pid == 1 {
            assert_eq!((x.0, x.1), (row.0, row.1), "the row of {pid}");
        } else {
            assert_eq!(x, row, "the row of {pid}");
        }
    }
    assert_eq!(a[&fixture].2, "kproc-fixture");
    assert_eq!(a[&fixture].1, me);
}

#[test]
fn a_processs_ids_start_and_age_are_the_ones_ps_prints() {
    let mut own = parked().process_group(0).spawn().expect("spawn");
    let led = tty_agent(&[], None);
    let me = std::process::id();
    let age = |pid: u32| -> u64 {
        aterm_time::unix_secs().expect("now")
            - aterm_sysprobe::kproc::record(pid).expect("record").start_s
    };
    for pid in [me, own.id(), led.id()] {
        let ps_ids = ps(&["-o", "ppid=,pgid=,tpgid=", "-p", &pid.to_string()]).expect("ps ids");
        let w: Vec<i64> = ps_ids
            .split_whitespace()
            .map(|n| n.parse().expect("number"))
            .collect();
        let read = ids(pid).expect("ids");
        assert_eq!(
            (i64::from(read.0), read.1, read.2),
            (w[0], w[1], w[2]),
            "{pid}"
        );
        let lstart = squash(&ps(&["-o", "lstart=", "-p", &pid.to_string()]).expect("ps lstart"));
        assert_eq!(kernel_start(pid).as_deref(), Some(lstart.as_str()), "{pid}");
        let before = age(pid);
        let etime = ps(&["-o", "etime=", "-p", &pid.to_string()]).expect("ps etime");
        let after = age(pid);
        let ps_age = parse_etime(etime.trim()).expect("etime");
        assert!(
            (before..=after).contains(&ps_age),
            "{pid}: {before} <= {ps_age} <= {after}"
        );
        assert!(started_ms(pid).is_some(), "{pid}");
    }
    let (own_id, led_id) = (own.id(), led.id());
    let _ = own.kill();
    let _ = own.wait();
    let mut led = led;
    let _ = led.kill();
    let _ = led.wait();
    assert_eq!(ids(own_id), None, "reaped: no process");
    assert_eq!(ps(&["-o", "pid=", "-p", &own_id.to_string()]), None);
    assert_eq!(kernel_start(led_id), None);
    assert_eq!(started_ms(led_id), None);
}

#[test]
fn the_terminals_owner_and_opener_are_the_ones_ps_reads() {
    use aterm_sysprobe::kproc;
    use std::ffi::OsStr;
    let me = std::process::id();
    let mut led = tty_agent(&[], None);
    let mut plain = parked().spawn().expect("spawn");
    let mut script = pty_host(&["/bin/sleep", "120"].map(OsStr::new))
        .spawn()
        .expect("spawn the pty host");
    let start = Instant::now();
    let slept = loop {
        if let Some((pid, _, _)) = table()
            .into_iter()
            .find(|(_, pp, name)| *pp == script.id() && name == "sleep")
        {
            break pid;
        }
        assert!(
            start.elapsed() < HANG,
            "the pty host's sleep: not within {HANG:?}"
        );
        std::thread::yield_now();
    };
    let host = ps_name(script.id()).expect("the host, as ps names it");
    let walk = |rows: &[kproc::Record]| -> BTreeMap<u32, Option<u32>> {
        let t: Vec<(u32, u32, String, String)> = rows
            .iter()
            .map(|r| (r.pid, r.ppid, tty_word(r.tty), String::new()))
            .collect();
        rows.iter()
            .map(|r| (r.pid, terminal_owner(r.pid, &t).map(|(o, _)| o)))
            .collect()
    };
    let rows_a = kproc::records().expect("records");
    let a = walk(&rows_a);
    let oracle = tty_table();
    let b = walk(&kproc::records().expect("records"));
    let fixtures = [led.id(), slept, plain.id(), me];
    let live: Vec<_> = fixtures.iter().map(|p| (*p, Live.terminal(*p))).collect();
    let opened = [
        (opener(led.id(), me), ps_opener(led.id(), me)),
        (opener(slept, script.id()), ps_opener(slept, script.id())),
        (opener(plain.id(), me), ps_opener(plain.id(), me)),
    ];
    // Only the fixtures, held still across the reads, are compared: any other process may
    // start, exec or end between them, and its terminal's owner with it.
    for pid in fixtures {
        let (Some(x), Some(y)) = (a.get(&pid), b.get(&pid)) else {
            panic!("fixture {pid}: in both reads");
        };
        assert_eq!(x, y, "fixture {pid} is held still");
        assert!(
            oracle.iter().any(|r| r.0 == pid),
            "fixture {pid}: in ps's table"
        );
        let want = terminal_owner(pid, &oracle).map(|(o, _)| o);
        assert_eq!(*x, want, "the owner of {pid}'s terminal");
    }
    for (pid, got) in &live {
        let want = terminal_owner(*pid, &oracle).map(|(o, name)| (o, name.to_string()));
        assert_eq!(*got, want, "{pid} live");
    }
    for (got, want) in &opened {
        assert_eq!(got, want, "the opener");
    }
    let script_id = script.id();
    let _ = led.kill();
    let _ = led.wait();
    for c in [&mut script, &mut plain] {
        let _ = c.kill();
        let _ = c.wait();
    }
    assert_eq!(
        live[0].1.as_ref().map(|(o, _)| *o),
        Some(me),
        "it leads a terminal this process opened"
    );
    assert_eq!(
        live[1].1.as_ref().map(|(o, n)| (*o, n.as_str())),
        Some((script_id, host.as_str())),
        "the pty's owner is the process that opened it, not this one"
    );
    assert_ne!(script_id, me);
    assert_eq!(
        opened[0].0,
        live[0].1.clone().map(|(_, n)| n),
        "the same opener"
    );
    assert_eq!(opened[1].0.as_deref(), Some(host.as_str()));
}

#[test]
fn what_runs_under_an_agent_is_named_as_ps_names_it() {
    let shell = login_shell();
    let me = std::process::id();
    let gone = {
        let mut c = parked().spawn().expect("spawn");
        let id = c.id();
        let _ = c.kill();
        let _ = c.wait();
        id
    };
    let procs = vec![
        (shell.id(), "bash".to_string()),
        (me, "aterm_agent".to_string()),
        (gone, "zsh".to_string()),
    ];
    let before = held_of(me, &procs);
    let oracle = ps_held_of(me, &procs);
    let after = held_of(me, &procs);
    end(shell);
    assert_eq!(oracle.len(), 2, "{oracle:?}");
    for got in [&before, &after] {
        assert_eq!(got.len(), oracle.len(), "{got:?} vs {oracle:?}");
        for (g, o) in got.iter().zip(&oracle) {
            assert_eq!((g.pid, &g.name, &g.command), (o.pid, &o.name, &o.command));
        }
    }
    for ((b, o), a) in before.iter().zip(&oracle).zip(&after) {
        assert!(
            (b.age_s..=a.age_s).contains(&o.age_s),
            "{}: {} <= {} <= {}",
            o.pid,
            b.age_s,
            o.age_s,
            a.age_s
        );
    }
    assert_eq!(
        before[0].command, "-kproc-fixture --noprofile --norc -c read x naM-CM-/ve l t\\011x",
        "the escaped newline read as a space (command_head)"
    );
    assert_eq!(before[1].command, "", "not the agent's own child");
}
