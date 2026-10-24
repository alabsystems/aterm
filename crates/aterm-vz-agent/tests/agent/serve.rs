// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The serve loop: `Exec`'s four ways to end kept apart, stdin, `merge`, the group a
//! dropped connection kills, `cwd` and exactly `env`, the clock, `Mount` and `Poweroff`.

use std::os::unix::fs::PermissionsExt as _;

use aterm_vz_agent::fake::Fake;
use aterm_vz_agent::serve::MAX_PENDING_STDIN_CHUNKS;
use aterm_vz_agent::system::{CLOCK_SLACK_NS, Identity, System as _};
use aterm_vz_agent::wire::{self, Exit, Frame, Share, Stdin};

use super::{Agent, Conn, HANG, test_pid, until};

/// Stop and join the synthetic input producer even when a response assertion fails.
struct Producer {
    stop: std::os::unix::net::UnixStream,
    done: std::sync::mpsc::Receiver<()>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl Producer {
    fn finish(&mut self) -> bool {
        let _ = self.stop.shutdown(std::net::Shutdown::Both);
        if self.worker.is_none() {
            return true;
        }
        if matches!(
            self.done.recv_timeout(HANG),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ) {
            drop(self.worker.take());
            return false;
        }
        self.worker.take().unwrap().join().is_ok()
    }
}

impl Drop for Producer {
    fn drop(&mut self) {
        if !self.finish() {
            eprintln!("hung: the closed stdin producer did not finish within {HANG:?}");
        }
    }
}

#[test]
fn an_exit_code_and_a_signal_are_reported_apart() {
    let agent = Agent::new(Fake::new());
    let ended = agent
        .request(&Frame::Exec(agent.stand_in(&["exit", "3"])))
        .ended();
    assert_eq!(ended.exit, Exit::Exited(3), "{}", ended.err());
    let ended = agent
        .request(&Frame::Exec(agent.stand_in(&["kill-self"])))
        .ended();
    assert_eq!(ended.exit, Exit::Signaled(9));

    let mut hang = agent.request(&Frame::Exec(agent.stand_in(&["hang", &test_pid()])));
    hang.out_until("pid ");
    hang.send(&Frame::Kill(15));
    assert_eq!(hang.ended().exit, Exit::Signaled(15));
}

#[test]
fn a_command_that_never_ran_is_not_started_and_one_that_exits_127_is_exited() {
    let agent = Agent::new(Fake::new());
    let missing = agent
        .request(&Frame::Exec(
            agent.exec(&[b"/nonexistent/aterm-vz-agent-absent"]),
        ))
        .ended();
    match &missing.exit {
        Exit::NotStarted(why) => assert!(
            why.contains("/nonexistent/aterm-vz-agent-absent") && why.contains("No such file"),
            "{why}"
        ),
        other => panic!("a missing program: {other:?}"),
    }
    let not_on_path = agent
        .request(&Frame::Exec(agent.exec(&[b"aterm-vz-agent-not-on-path"])))
        .ended();
    assert!(
        matches!(not_on_path.exit, Exit::NotStarted(_)),
        "{not_on_path:?}"
    );

    let mut in_absent_dir = agent.stand_in(&["exit", "0"]);
    let absent = agent.dir.path().join("absent");
    in_absent_dir.cwd = absent.to_string_lossy().as_bytes().to_vec();
    let ended = agent.request(&Frame::Exec(in_absent_dir)).ended();
    match &ended.exit {
        Exit::NotStarted(why) => assert!(
            why.starts_with("cwd ") && why.contains(&*absent.to_string_lossy()),
            "{why}"
        ),
        other => panic!("a missing cwd: {other:?}"),
    }

    let plain = agent.dir.path().join("plain");
    std::fs::write(&plain, b"#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&plain, std::fs::Permissions::from_mode(0o644)).unwrap();
    let ended = agent
        .request(&Frame::Exec(
            agent.exec(&[plain.to_string_lossy().as_bytes()]),
        ))
        .ended();
    match &ended.exit {
        Exit::NotStarted(why) => assert!(why.contains("Permission denied"), "{why}"),
        other => panic!("a file with no exec permission: {other:?}"),
    }

    // The negative control: a program that runs and exits 127 by itself is Exited.
    let ended = agent
        .request(&Frame::Exec(agent.stand_in(&["exit", "127"])))
        .ended();
    assert_eq!(ended.exit, Exit::Exited(127));

    let empty = agent.request(&Frame::Exec(agent.exec(&[]))).ended();
    assert_eq!(empty.exit, Exit::NotStarted("an empty argv".into()));
}

#[test]
fn stdin_reaches_the_command_and_its_eof_ends_it() {
    let agent = Agent::new(Fake::new());
    let mut exec = agent.stand_in(&["echo"]);
    exec.stdin = Stdin::Pipe;
    let mut c = agent.request(&Frame::Exec(exec));
    c.send(&Frame::Stdin(b"hello ".to_vec()));
    c.send(&Frame::Stdin(Vec::new()));
    c.send(&Frame::Stdin(b"world\n".to_vec()));
    c.send(&Frame::StdinEof);
    let ended = c.ended();
    assert_eq!(ended.exit, Exit::Exited(0));
    assert_eq!(ended.out(), "hello world\n");

    // A null stdin reads EOF at once; bytes sent for it go nowhere.
    let mut c = agent.request(&Frame::Exec(agent.stand_in(&["echo"])));
    c.send(&Frame::Stdin(b"dropped".to_vec()));
    let ended = c.ended();
    assert_eq!(
        (ended.exit.clone(), ended.out()),
        (Exit::Exited(0), String::new())
    );

    // This real command never reads stdin. Its pid is the acknowledgement that it ran.
    let stalled = || {
        let mut exec = agent.stand_in(&["hang", &test_pid()]);
        exec.stdin = Stdin::Pipe;
        let mut conn = agent.request(&Frame::Exec(exec));
        let said = conn.out_until("\n");
        let pid = said
            .trim()
            .strip_prefix("pid ")
            .unwrap()
            .parse::<u32>()
            .unwrap();
        (conn, pid)
    };
    let large = Frame::Stdin(vec![7; wire::MAX_FRAME]);
    let (mut held, pid) = stalled();
    held.send(&large);
    held.send(&Frame::StdinEof);
    held.send(&Frame::Kill(9));
    let ended = held.ended();
    assert_eq!(ended.exit, Exit::Signaled(9));
    assert!(!ended.err().contains("pending input limit"));
    until("the stalled command reaped after Kill", || {
        (!aterm_uds::process::pid_alive(pid)).then_some(())
    });

    let (mut held, pid) = stalled();
    held.send(&large);
    drop(held);
    until("the stalled command reaped after peer EOF", || {
        (!aterm_uds::process::pid_alive(pid)).then_some(())
    });

    let (held, pid) = stalled();
    let mut producer = held.0.try_clone().unwrap();
    producer.set_write_timeout(Some(HANG)).unwrap();
    let stop = held.0.try_clone().unwrap();
    let (done, produced) = std::sync::mpsc::channel();
    let sent = std::thread::spawn(move || {
        // A finite burst, not a wait for pipe latency. The gated worker unit control
        // proves both exact admission bounds independently of a kernel's pipe size.
        for _ in 0..=MAX_PENDING_STDIN_CHUNKS {
            if wire::write_frame(&mut producer, &large).is_err() {
                break;
            }
        }
        let _ = done.send(());
    });
    let mut producer = Producer {
        stop,
        done: produced,
        worker: Some(sent),
    };
    let ended = Conn::new(held.0).ended();
    assert!(
        producer.finish(),
        "hung: the closed input producer did not end"
    );
    assert_eq!(ended.exit, Exit::Signaled(9));
    assert!(
        ended.err().contains("stdin pending input limit exceeded"),
        "{}",
        ended.err()
    );
    until("the overloaded command reaped", || {
        (!aterm_uds::process::pid_alive(pid)).then_some(())
    });
}

#[test]
fn merge_keeps_the_guests_order_and_apart_keeps_the_streams_apart() {
    let agent = Agent::new(Fake::new());
    let n = 200;
    let mut merged = agent.stand_in(&["interleave", &n.to_string()]);
    merged.merge = true;
    let ended = agent.request(&Frame::Exec(merged)).ended();
    assert_eq!(ended.exit, Exit::Exited(0));
    let want: String = (0..n).map(|i| format!("out {i}\nerr {i}\n")).collect();
    assert_eq!(ended.out(), want, "one pipe: the order the guest wrote");
    assert!(ended.err.is_empty());

    let ended = agent
        .request(&Frame::Exec(
            agent.stand_in(&["interleave", &n.to_string()]),
        ))
        .ended();
    let outs: String = (0..n).map(|i| format!("out {i}\n")).collect();
    let errs: String = (0..n).map(|i| format!("err {i}\n")).collect();
    assert_eq!((ended.out(), ended.err()), (outs, errs));
}

#[test]
fn a_dropped_connection_kills_the_commands_group() {
    let agent = Agent::new(Fake::new());
    let mut c = agent.request(&Frame::Exec(agent.stand_in(&["group", &test_pid()])));
    let said = c.out_until("\n");
    let pids: Vec<u32> = said
        .trim()
        .strip_prefix("pids ")
        .unwrap_or_else(|| panic!("{said:?}"))
        .split(' ')
        .map(|p| p.parse().unwrap())
        .collect();
    assert_eq!(pids.len(), 2, "{said:?}");
    for pid in &pids {
        assert!(
            aterm_uds::process::pid_alive(*pid),
            "{pid} runs before the drop"
        );
    }
    drop(c);
    for pid in pids {
        until(&format!("the group's {pid} killed"), || {
            (!aterm_uds::process::pid_alive(pid)).then_some(())
        });
    }
}

#[test]
fn a_command_gets_its_cwd_and_exactly_its_env() {
    let agent = Agent::new(Fake::new());
    let mut exec = agent.stand_in(&["env"]);
    let cwd = std::fs::canonicalize(agent.dir.path()).unwrap();
    exec.cwd = cwd.to_string_lossy().as_bytes().to_vec();
    exec.env = vec![
        (b"HOME".to_vec(), b"/home/me.guest".to_vec()),
        (b"EMPTY".to_vec(), Vec::new()),
    ];
    let ended = agent.request(&Frame::Exec(exec)).ended();
    assert_eq!(ended.exit, Exit::Exited(0), "{}", ended.err());
    assert_eq!(
        ended.out(),
        format!("EMPTY=\nHOME=/home/me.guest\ncwd={}\n", cwd.display()),
        "nothing of the agent's environment reaches the command"
    );
}

#[test]
fn a_request_steps_a_clock_past_the_slack_and_leaves_one_within_it() {
    let agent = Agent::new(Fake::new());
    let guest = agent.fake.now_unix_ns();
    let mut within = agent.stand_in(&["exit", "0"]);
    within.clock_unix_ns = guest + CLOCK_SLACK_NS;
    agent.request(&Frame::Exec(within)).ended();
    assert_eq!(agent.fake.clock_sets(), Vec::<u64>::new());
    let mut behind = agent.stand_in(&["exit", "0"]);
    let host = guest + 120_000_000_000;
    behind.clock_unix_ns = host;
    agent.request(&Frame::Exec(behind)).ended();
    assert_eq!(
        agent.fake.clock_sets(),
        [host],
        "a guest two minutes behind (no RTC)"
    );
}

#[test]
fn mount_and_poweroff_answer_with_one_exit() {
    let agent = Agent::new(Fake::new());
    let shares = vec![Share {
        name: "repo".into(),
        path: b"/Users//me/aterm".to_vec(),
    }];
    let ended = agent.request(&Frame::Mount(shares.clone())).ended();
    assert_eq!(ended.exit, Exit::Exited(0));
    assert_eq!(agent.fake.mounts(), std::slice::from_ref(&shares));
    let ended = agent.request(&Frame::Poweroff).ended();
    assert_eq!(ended.exit, Exit::Exited(0));
    assert_eq!(agent.fake.poweroffs(), 1);

    let refusing = Agent::new(Fake::new().refuse_mounts("mount: Operation not permitted"));
    let ended = refusing.request(&Frame::Mount(shares)).ended();
    assert_eq!(
        ended.exit,
        Exit::NotStarted("mount: Operation not permitted".into())
    );

    let ended = agent.request(&Frame::Stdin(b"x".to_vec())).ended();
    match ended.exit {
        Exit::NotStarted(why) => assert!(why.contains("not a request"), "{why}"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn no_lane_user_or_a_switch_off_the_guest_never_runs_the_command() {
    let none = Agent::new(Fake::new().identity(Err("no lane user: none".into())));
    let ended = none
        .request(&Frame::Exec(none.stand_in(&["exit", "0"])))
        .ended();
    assert_eq!(ended.exit, Exit::NotStarted("no lane user: none".into()));
    #[cfg(not(target_os = "linux"))]
    {
        let switch = Agent::new(Fake::new().identity(Ok(Identity::Switch {
            uid: 65534,
            gid: 65534,
            groups: vec![65534],
        })));
        let ended = switch
            .request(&Frame::Exec(switch.stand_in(&["exit", "0"])))
            .ended();
        match ended.exit {
            Exit::NotStarted(why) => assert!(why.contains("only in the Linux guest"), "{why}"),
            other => panic!("ran as the agent's own user: {other:?}"),
        }
    }
}

/// The identity switch itself needs root, so everywhere else it is a named skip: an
/// ignored test on every platform, which off the Linux guest's root fails if run.
#[test]
#[ignore = "needs root in the Linux guest: the identity switch (setgroups, setgid, setuid, then the cwd's chdir) runs as the guest's root; run it there with --ignored"]
fn the_identity_switch_runs_the_command_as_the_lane_user_in_its_groups_and_cwd() {
    #[cfg(target_os = "linux")]
    as_the_lane_user();
    #[cfg(not(target_os = "linux"))]
    panic!("NOT RUN: the identity switch runs only as the Linux guest's root");
}

/// The identity switch's test body: the guest's root only.
#[cfg(target_os = "linux")]
fn as_the_lane_user() {
    let agent = Agent::new(Fake::new().identity(Ok(Identity::Switch {
        uid: 65534,
        gid: 65534,
        groups: vec![65534, 100],
    })));
    let ended = agent
        .request(&Frame::Exec(agent.stand_in(&["ids"])))
        .ended();
    assert_eq!(ended.exit, Exit::Exited(0), "{}", ended.err());
    let out = ended.out();
    assert!(out.contains("Uid:\t65534\t65534\t65534\t65534"), "{out}");
    assert!(out.contains("Gid:\t65534\t65534\t65534\t65534"), "{out}");
    assert!(out.contains("Groups:\t100 65534"), "{out}");

    // The cwd is entered as the lane user: one only root may enter is refused (lima's
    // `cd` as the user), and the same directory opened to everyone is entered.
    std::fs::set_permissions(agent.dir.path(), std::fs::Permissions::from_mode(0o711)).unwrap();
    let inside = agent.dir.path().join("root-only");
    std::fs::create_dir(&inside).unwrap();
    std::fs::set_permissions(&inside, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut exec = agent.stand_in(&["env"]);
    exec.cwd = inside.to_string_lossy().as_bytes().to_vec();
    let ended = agent.request(&Frame::Exec(exec.clone())).ended();
    match &ended.exit {
        Exit::NotStarted(why) => assert!(
            why.contains("Permission denied") && why.contains(&*inside.to_string_lossy()),
            "{why}"
        ),
        other => panic!("entered a root-only cwd as uid 65534: {other:?}"),
    }
    std::fs::set_permissions(&inside, std::fs::Permissions::from_mode(0o755)).unwrap();
    let ended = agent.request(&Frame::Exec(exec)).ended();
    assert_eq!(ended.exit, Exit::Exited(0), "{}", ended.err());
    assert!(
        ended
            .out()
            .ends_with(&format!("cwd={}\n", inside.display())),
        "{}",
        ended.out()
    );
}
