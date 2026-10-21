// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE PROCESS READS ARE `ps`'s, on the live machine: macOS reads the
//! kernel's records in-process (`aterm_sysprobe::kproc`), and every reader
//! the host's job and restart rest on — the table, a process's ids, start and
//! age, its terminal's owner and opener, what runs under an agent — is held
//! here to the `ps` read it replaced (kept as the oracle: `ps_table`,
//! `tty_table`, `ps_opener`, `ps_held_of`, `ps`).
//!
//! The machine runs on while a test reads it: a row is compared only where
//! two kernel reads that sandwich the `ps` read agree, and each test asserts
//! its fixtures were compared and that most rows were, so none passes having
//! compared nothing.

use std::collections::BTreeMap;

use aterm_uds::spawnfd::{Child, Stdio};

use super::*;

/// The hang detector for what must happen (an exec).
const HANG: Duration = Duration::from_secs(60);

/// A `bash` whose `argv[0]` is a login shell's (`-kproc-fixture`), blocked
/// in `read` until its stdin closes, its words holding every escape `ps`
/// makes.
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

/// Rows keyed by pid.
fn keyed<R: Clone>(rows: &[R], pid: impl Fn(&R) -> u32) -> BTreeMap<u32, R> {
    rows.iter().map(|r| (pid(r), r.clone())).collect()
}

/// THE TABLE IS THE ONE `ps` PRINTS: pid, parent and the `comm` column's
/// basename, a login shell's `-` dropped, for every process `ps` lists that
/// held still across the read. Another user's process is named by its
/// image's path (its `argv[0]` is `ps`'s alone to read, setuid root), so only
/// its pid and parent are held to `ps`'s.
#[test]
fn the_table_is_the_one_ps_prints() {
    let shell = login_shell();
    // SAFETY: `geteuid` reads this process's effective uid.
    let uid = unsafe { libc::geteuid() };
    let mine: std::collections::BTreeSet<u32> = aterm_sysprobe::kproc::records()
        .expect("records")
        .iter()
        .filter(|r| r.uid == uid)
        .map(|r| r.pid)
        .collect();
    // Each name is read when first asked for (`Comm`): asked here, so both
    // reads sandwich the `ps` read.
    let named = || {
        let t = table();
        for (_, _, name) in &t {
            name.as_str();
        }
        keyed(&t, |r| r.0)
    };
    let a = named();
    let oracle = ps_table();
    let b = named();
    let fixture = shell.id();
    end(shell);
    let mut compared = 0;
    for row in &oracle {
        let (Some(x), Some(y)) = (a.get(&row.0), b.get(&row.0)) else {
            continue;
        };
        if x != y {
            continue;
        }
        if mine.contains(&row.0) {
            assert_eq!(x, row, "the row of {}", row.0);
        } else {
            assert_eq!((x.0, x.1), (row.0, row.1), "the row of {}", row.0);
        }
        compared += 1;
    }
    assert!(mine.contains(&fixture));
    assert_eq!(a[&fixture].2, "kproc-fixture");
    assert_eq!(a[&fixture].1, std::process::id());
    assert!(a.contains_key(&std::process::id()));
    assert!(
        compared * 2 >= oracle.len(),
        "{compared} of {} rows compared",
        oracle.len()
    );
}

/// A PROCESS'S IDS AND START ARE THE ONES `ps` PRINTS, and its age (what
/// [`started_ms`] and [`held_of`] count from) lies between two readings
/// that sandwich `ps`'s `etime`: this process, a child in a group of its own
/// (a job, not the terminal's foreground) and one on a terminal it leads.
/// A pid nothing runs as reads nothing, as `ps -p` prints nothing.
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

/// THE TERMINAL'S OWNER AND OPENER ARE THE ONES `ps`'s TABLE GIVES: the walk
/// up from every process that held still, over one read of the kernel's
/// records, names the owner the walk over `ps -A -o pid=,ppid=,tty=,comm=`
/// names; the fixtures — a stand-in leading a terminal this process opened,
/// a `sleep` on the pty `script` opened, a child on no terminal — are always
/// compared, live too, and so is the two-process opener read.
#[test]
fn the_terminals_owner_and_opener_are_the_ones_ps_reads() {
    use aterm_sysprobe::kproc;
    let me = std::process::id();
    let mut led = tty_agent(&[], None);
    let mut plain = parked().spawn().expect("spawn");
    let mut script = Command::new("/usr/bin/script")
        .args(["-q", "/dev/null", "/bin/sleep", "120"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn script");
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
            "script's sleep: not within {HANG:?}"
        );
        std::thread::yield_now();
    };
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
    let mut compared = 0;
    let mut seen = Vec::new();
    for (pid, _, _, _) in &oracle {
        let (Some(x), Some(y)) = (a.get(pid), b.get(pid)) else {
            continue;
        };
        if x != y {
            continue;
        }
        let want = terminal_owner(*pid, &oracle).map(|(o, _)| o);
        assert_eq!(*x, want, "the owner of {pid}'s terminal");
        compared += 1;
        seen.push(*pid);
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
    for pid in fixtures {
        assert!(seen.contains(&pid), "fixture {pid} compared");
    }
    assert_eq!(
        live[0].1.as_ref().map(|(o, _)| *o),
        Some(me),
        "it leads a terminal this process opened"
    );
    assert_eq!(
        live[1].1.as_ref().map(|(o, n)| (*o, n.as_str())),
        Some((script_id, "script"))
    );
    assert_eq!(
        opened[0].0,
        live[0].1.clone().map(|(_, n)| n),
        "the same opener"
    );
    assert_eq!(opened[1].0.as_deref(), Some("script"));
    assert!(
        compared * 2 >= oracle.len(),
        "{compared} of {} rows compared",
        oracle.len()
    );
}

/// WHAT RUNS UNDER AN AGENT IS NAMED AS `ps` NAMES IT: the same processes,
/// the agent's own child's command head out of its `args` (escaped as `ps`
/// escapes them in the C locale), a grandchild's none, each one's age
/// between two readings that sandwich `ps`'s; a process gone since the walk
/// is left out of both.
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
