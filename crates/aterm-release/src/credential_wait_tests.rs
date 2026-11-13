// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const LISTING: &str = "\
      1     0 /sbin/launchd\n\
    500     1 git -C /x fetch --quiet --no-tags origin refs/heads/main\n\
    501   500 /opt/homebrew/opt/git/libexec/git-core/git remote-https origin https://h/r\n\
    502   501 git credential-osxkeychain get\n\
    503   502 /opt/homebrew/opt/git/libexec/git-core/git-credential-osxkeychain get\n\
    504   500 git-credential-cache--daemon /tmp/socket\n\
    505   500 git credential fill\n\
    506   500 /bin/sh -c gh auth git-credential get\n\
    507   500 git credential- get\n\
    508   500 /bin/sh /abs/git-credential-x get\n\
    509   508 sleep 120\n\
    510   500 perl -w /abs/git-credential-netrc get\n\
    511   500 /usr/bin/env PYTHONUNBUFFERED=1 python3 /abs/git-credential-y get\n\
    512   500 /bin/sh /abs/git-credential-cache--daemon /tmp/socket\n\
    513   500 /bin/bash -lc git-credential-z get\n\
    600     1 git credential-store get\n\
    garbage line\n";

fn named(pids: &[(i32, &str)]) -> Vec<(i32, String)> {
    pids.iter().map(|(p, n)| (*p, (*n).to_string())).collect()
}

/// `pid ppid args` lines as [`Row`]s; a line that does not start with two numbers is skipped.
fn rows(listing: &str) -> Vec<Row> {
    listing
        .lines()
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            let pid = words.next()?.parse().ok()?;
            let ppid = words.next()?.parse().ok()?;
            Some(Row {
                pid,
                ppid,
                args: words.collect::<Vec<_>>().join(" "),
            })
        })
        .collect()
}

/// One look reads the arguments of the processes below the root and of those it ended, and
/// of no other process.
#[test]
fn a_look_reads_only_the_arguments_of_the_roots_tree_and_what_it_ended() {
    let procs = [
        (1, 0),
        (500, 1),
        (501, 500),
        (502, 501),
        (600, 1),
        (601, 600),
        (509, 1),
    ];
    let mut read = Vec::new();
    let rows = table_of(procs, 500, &[509], |pid| {
        read.push(pid);
        format!("words of {pid}")
    });
    read.sort_unstable();
    assert_eq!(read, [500, 501, 502, 509]);
    assert_eq!(rows.len(), procs.len(), "every process keeps its row");
    for row in &rows {
        let wanted = [500, 501, 502, 509].contains(&row.pid);
        assert_eq!(row.args.is_empty(), !wanted, "{row:?}");
    }
}

/// This process's own arguments read in-process, as `ps -o args=` prints them for a plain
/// argument vector; a pid no process holds reads as none.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_live_processes_arguments_read_in_process() {
    let me = i32::try_from(std::process::id()).expect("own pid");
    let words: Vec<String> = std::env::args().collect();
    assert_eq!(args_of(me), words.join(" "));
    assert_eq!(args_of(i32::MAX), "");
}

#[test]
fn a_helper_is_the_topmost_git_credential_process_under_the_root() {
    assert_eq!(
        helpers(&rows(LISTING), 500),
        named(&[(502, "osxkeychain"), (508, "x"), (510, "netrc"), (511, "y")])
    );
    assert_eq!(helpers(&rows(LISTING), 600), named(&[(600, "store")]));
    assert!(helpers(&rows(LISTING), 999).is_empty());
}

#[test]
fn ending_a_helper_reaches_everything_it_started() {
    assert_eq!(
        subtree(&rows(LISTING), 502),
        named(&[
            (502, "git credential-osxkeychain get"),
            (
                503,
                "/opt/homebrew/opt/git/libexec/git-core/git-credential-osxkeychain get"
            ),
        ])
    );
    assert_eq!(
        subtree(&rows(LISTING), 508),
        named(&[
            (508, "/bin/sh /abs/git-credential-x get"),
            (509, "sleep 120")
        ])
    );
}

#[test]
fn a_helper_is_overdue_once_it_has_run_the_whole_wait() {
    let wait = Duration::from_secs(60);
    let t0 = Instant::now();
    let at = |s: u64| t0 + Duration::from_secs(s);
    let process = |pid, boot| Identity {
        pid,
        start: ProcessStart {
            boot: aterm_uds::process::BootClock(boot),
            wall: None,
        },
    };
    let one = |pid: i32| vec![(process(pid, 10), "osxkeychain".to_string())];
    let mut seen = Seen::default();
    assert!(seen.overdue(one(502), t0, wait).is_empty());
    assert!(seen.overdue(one(502), at(59), wait).is_empty());
    assert_eq!(seen.overdue(one(502), at(60), wait), one(502));
    assert!(seen.overdue(Vec::new(), at(61), wait).is_empty());
    assert!(seen.overdue(one(502), at(62), wait).is_empty());
    assert!(seen.overdue(one(502), at(121), wait).is_empty());
    assert_eq!(seen.overdue(one(502), at(122), wait), one(502));
    let other = vec![(process(502, 10), "store".to_string())];
    assert!(seen.overdue(other, at(123), wait).is_empty());
    let reused = vec![(process(502, 20), "store".to_string())];
    assert!(seen.overdue(reused.clone(), at(182), wait).is_empty());
    assert!(seen.overdue(reused.clone(), at(241), wait).is_empty());
    assert_eq!(seen.overdue(reused, at(242), wait).len(), 1);
    let ended = process(503, 10);
    assert!(ended.matches(Some(process(503, 10).start)));
    assert!(!ended.matches(Some(process(503, 20).start)));
    assert!(!ended.matches(None));
    let mut ended = vec![
        (process(503, 10), "sleep 120".to_string()),
        (process(509, 10), "sleep 120".to_string()),
        (process(510, 10), "sleep 120".to_string()),
    ];
    let mut signaled = Vec::new();
    escalate(
        &mut ended,
        &rows("503 999 sleep 120\n509 1 sleep 120\n510 1 sleep 120\n"),
        |pid| match pid {
            503 => Some(process(pid, 20).start),
            509 => Some(process(pid, 10).start),
            510 => None,
            _ => panic!("no root or unrelated identity is needed"),
        },
        |pid| signaled.push(pid),
    );
    assert_eq!(signaled, [509]);
    assert_eq!(ended, [(process(509, 10), "sleep 120".to_string())]);
}

/// A process ended while its arguments were unread (its thread had not run) is still killed
/// once they read, its start alone naming it then, and dropped if it execs after that.
/// NEGATIVE CONTROL: compare the unread words with those read, and it is dropped unsignalled.
#[test]
fn a_process_ended_unread_is_still_killed_once_its_arguments_read() {
    let start = ProcessStart {
        boot: aterm_uds::process::BootClock(10),
        wall: None,
    };
    let process = Identity { pid: 503, start };
    let mut ended = vec![(process, String::new())];
    let mut signaled = Vec::new();
    escalate(
        &mut ended,
        &rows("503 1 security find-generic-password -w\n"),
        |_| Some(start),
        |pid| signaled.push(pid),
    );
    assert_eq!(signaled, [503], "killed once it runs");
    assert_eq!(
        ended,
        [(process, "security find-generic-password -w".to_string())]
    );
    escalate(
        &mut ended,
        &rows("503 1 /bin/zsh\n"),
        |_| Some(start),
        |pid| signaled.push(pid),
    );
    assert_eq!(signaled, [503], "an exec after its words were read is left");
    assert!(ended.is_empty());
}

#[test]
fn the_releases_git_never_prompts_on_a_terminal() {
    use crate::ledger::GitRunner as _;
    let dir = aterm_tempfile::tempdir().expect("scratch dir");
    let ask = [
        "-c",
        "alias.ask=!printf '%s [%s]' \"$GIT_TERMINAL_PROMPT\" \"${GIT_ASKPASS-unset}\"",
        "ask",
    ];
    let git = crate::ledger::GitCli::new(dir.path());
    for out in [git.git(&ask), git.git_input(&ask, b"")] {
        let out = out.expect("run git");
        assert_eq!(out.stdout_utf8().trim(), "0 []", "{}", out.stderr_utf8());
    }
}

fn give_up(group: i32, why: &str) -> ! {
    end(-group, true);
    panic!("{why}")
}

/// A REAL GIT, ASKING A HELPER THAT NEVER ANSWERS.
#[cfg(unix)]
#[test]
fn a_helper_waiting_for_a_person_is_ended_with_what_it_started() {
    use aterm_uds::spawnfd::{Stdio, command};
    use std::io::Write as _;

    let dir = aterm_tempfile::tempdir().expect("scratch dir");
    aterm_tempfile::stand_in(
        &dir.path().join("git-credential-silent"),
        "#!/bin/sh\n: > ran\nsleep 120\n",
    )
    .expect("lay the helper");
    let path = format!(
        "{}:{}",
        dir.path().display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let askpass = dir.path().join("askpass");
    aterm_tempfile::stand_in(&askpass, "#!/bin/sh\n: > asked\necho answer\n")
        .expect("lay the askpass");
    let core_askpass = format!("core.askPass={}", askpass.display());
    let mut git = command(aterm_uds::os_path!("git"));
    let mut child = ask_no_one(&mut git)
        .current_dir(dir.path())
        .env("PATH", path)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("SSH_ASKPASS", &askpass)
        .args([
            "-c",
            &core_askpass,
            "-c",
            "credential.helper=",
            "-c",
            "credential.helper=silent",
            "credential",
            "fill",
        ])
        .process_group(0)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn git");
    let group = i32::try_from(child.id()).expect("git's pid");
    let mut stdin = child.stdin.take().expect("git's stdin");
    stdin
        .write_all(b"protocol=https\nhost=example.invalid\n\n")
        .expect("ask git");
    drop(stdin);
    let bound = Instant::now() + Duration::from_secs(60);
    while !dir.path().join("ran").exists() {
        if Instant::now() > bound {
            give_up(group, "git never ran its helper");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let watch = Watch::with(
        child.id(),
        Duration::from_millis(300),
        Duration::from_millis(50),
    );
    let bound = Instant::now() + Duration::from_secs(60);
    let status = loop {
        if let Some(status) = child.try_wait().expect("wait for git") {
            break status;
        }
        if Instant::now() > bound {
            give_up(
                group,
                "git still waits: the watch never ended its helper, or left a child of it \
                 holding git's pipe",
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    drop(watch);
    assert!(
        !dir.path().join("asked").exists(),
        "git ran an askpass after its helper was ended"
    );
    assert_eq!(
        status.code(),
        Some(128),
        "git did not fail on its own: the watch ended git, not its helper"
    );
}
