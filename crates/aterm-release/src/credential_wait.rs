// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A GIT CREDENTIAL HELPER WAITING FOR A PERSON IS ENDED, NOT WAITED ON.

use std::collections::BTreeSet;
use std::io::Write as _;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use aterm_uds::process::{ProcessStart, read_process_start};

/// Make `cmd`'s git ask no person.
pub fn ask_no_one(cmd: &mut aterm_uds::spawnfd::Command) -> &mut aterm_uds::spawnfd::Command {
    cmd.env("GIT_TERMINAL_PROMPT", "0").env("GIT_ASKPASS", "")
}

/// How long a git credential helper may run before it is taken for one waiting on a person.
pub const PERSON_WAIT: Duration = Duration::from_secs(60);

const LOOK: Duration = Duration::from_secs(5);

struct Row<'a> {
    pid: i32,
    ppid: i32,
    args: &'a str,
}

fn rows(listing: &str) -> Vec<Row<'_>> {
    listing
        .lines()
        .filter_map(|line| {
            let mut rest = line.trim_start();
            let mut num = || -> Option<i32> {
                let end = rest.find(char::is_whitespace)?;
                let n = rest[..end].parse().ok()?;
                rest = rest[end..].trim_start();
                Some(n)
            };
            let (pid, ppid) = (num()?, num()?);
            Some(Row {
                pid,
                ppid,
                args: rest.trim_end(),
            })
        })
        .collect()
}

fn below(rows: &[Row<'_>], top: i32) -> BTreeSet<i32> {
    let mut tree = BTreeSet::from([top]);
    loop {
        let before = tree.len();
        for row in rows {
            if tree.contains(&row.ppid) {
                tree.insert(row.pid);
            }
        }
        if tree.len() == before {
            return tree;
        }
    }
}

/// The git credential helpers running under `root` in a `ps -A -o pid=,ppid=,args=` listing.
#[must_use]
pub fn helpers(listing: &str, root: i32) -> Vec<(i32, String)> {
    let rows = rows(listing);
    let tree = below(&rows, root);
    let named: Vec<(&Row<'_>, &str)> = rows
        .iter()
        .filter(|row| tree.contains(&row.pid))
        .filter_map(|row| helper_name(row.args).map(|name| (row, name)))
        .collect();
    let is_helper = |pid: i32| named.iter().any(|(row, _)| row.pid == pid);
    let parent = |pid: i32| rows.iter().find(|row| row.pid == pid).map(|row| row.ppid);
    let under_a_helper = |row: &Row<'_>| {
        let mut at = row.ppid;
        for _ in 0..rows.len() {
            if is_helper(at) {
                return true;
            }
            match parent(at) {
                Some(up) if at != root => at = up,
                _ => return false,
            }
        }
        false
    };
    named
        .iter()
        .filter(|(row, _)| !under_a_helper(row))
        .map(|(row, name)| (row.pid, (*name).to_string()))
        .collect()
}

/// `top` and every process below it in a `ps -A -o pid=,ppid=,args=` listing.
#[must_use]
pub fn subtree(listing: &str, top: i32) -> Vec<(i32, String)> {
    let rows = rows(listing);
    let tree = below(&rows, top);
    rows.iter()
        .filter(|row| tree.contains(&row.pid))
        .map(|row| (row.pid, row.args.to_string()))
        .collect()
}

fn helper_name(args: &str) -> Option<&str> {
    let mut words = args.split_whitespace();
    let mut program = basename(words.next()?);
    while interpreter(program) {
        let env = program == "env";
        program = loop {
            let word = words.next()?;
            if let Some(flags) = word.strip_prefix('-') {
                if !flags.starts_with('-') && flags.contains('c') {
                    return None;
                }
                continue;
            }
            if env && word.contains('=') {
                continue;
            }
            break basename(word);
        };
    }
    let name = if program == "git" {
        words.next()?.strip_prefix("credential-")?
    } else {
        program.strip_prefix("git-credential-")?
    };
    (!name.is_empty() && !name.ends_with("--daemon")).then_some(name)
}

fn basename(word: &str) -> &str {
    word.rsplit('/').next().unwrap_or(word)
}

fn interpreter(program: &str) -> bool {
    matches!(
        program,
        "sh" | "bash" | "dash" | "zsh" | "ksh" | "env" | "ruby" | "node"
    ) || program.starts_with("perl")
        || program.to_ascii_lowercase().starts_with("python")
}

/// When each helper in a child's tree was first seen, by identity and name.
#[derive(Debug, Default)]
struct Seen(Vec<(Identity, String, Instant)>);

/// A pid names a process only while its kernel start record still agrees.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Identity {
    pid: i32,
    start: ProcessStart,
}

impl Identity {
    fn read(pid: i32) -> Option<Self> {
        Some(Self {
            pid,
            start: read_process_start(pid)?,
        })
    }

    fn matches(self, start: Option<ProcessStart>) -> bool {
        start == Some(self.start)
    }

    fn current(self) -> bool {
        self.matches(read_process_start(self.pid))
    }
}

impl Seen {
    fn overdue(
        &mut self,
        running: Vec<(Identity, String)>,
        now: Instant,
        wait: Duration,
    ) -> Vec<(Identity, String)> {
        self.0
            .retain(|(pid, name, _)| running.iter().any(|(p, n)| p == pid && n == name));
        for (pid, name) in running {
            if !self.0.iter().any(|(p, n, _)| *p == pid && *n == name) {
                self.0.push((pid, name, now));
            }
        }
        self.0
            .iter()
            .filter(|(.., since)| now.saturating_duration_since(*since) >= wait)
            .map(|(pid, name, _)| (*pid, name.clone()))
            .collect()
    }
}

/// The watch on one child's credential helpers, for as long as this value lives.
pub struct Watch {
    stop: Option<mpsc::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Watch {
    /// Watch `root`, a child just spawned.
    #[must_use]
    pub fn enter(root: u32) -> Self {
        Self::with(root, PERSON_WAIT, LOOK)
    }

    fn with(root: u32, wait: Duration, look: Duration) -> Self {
        let inert = Self {
            stop: None,
            thread: None,
        };
        if !cfg!(unix) {
            return inert;
        }
        let Ok(root) = i32::try_from(root) else {
            return inert;
        };
        let Some(root) = Identity::read(root) else {
            return inert;
        };
        let (stop, stopped) = mpsc::channel::<()>();
        let thread = std::thread::Builder::new()
            .name("credential-wait".to_string())
            .spawn(move || watch(root, wait, look, &stopped))
            .ok();
        Self {
            stop: Some(stop),
            thread,
        }
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        self.stop = None;
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn watch(root: Identity, wait: Duration, look: Duration, stopped: &mpsc::Receiver<()>) {
    let mut seen = Seen::default();
    let mut ended: Vec<(Identity, String)> = Vec::new();
    while let Err(mpsc::RecvTimeoutError::Timeout) = stopped.recv_timeout(look) {
        if !root.current() && ended.is_empty() {
            return;
        }
        let Some(listing) = listing() else {
            continue;
        };
        escalate(&mut ended, &listing, read_process_start, |pid| {
            end(pid, true)
        });
        if !root.current() {
            continue;
        }
        let running = helpers(&listing, root.pid)
            .into_iter()
            .filter_map(|(pid, name)| Identity::read(pid).map(|process| (process, name)))
            .collect();
        for (helper, name) in seen.overdue(running, Instant::now(), wait) {
            if ended.iter().any(|(p, _)| *p == helper) {
                continue;
            }
            let reached: Vec<_> = subtree(&listing, helper.pid)
                .into_iter()
                .filter_map(|(pid, args)| Identity::read(pid).map(|process| (process, args)))
                .collect();
            if !helper.current() {
                continue;
            }
            for (process, args) in reached {
                if root.current() && process.current() {
                    end(process.pid, false);
                    ended.push((process, args));
                }
            }
            let pid = helper.pid;
            let _ = writeln!(
                std::io::stderr(),
                "aterm-release: ended git's credential helper `{name}` (pid {pid}) and what it \
                 started, after {}s: one still running that long is waiting for a person (a \
                 locked keychain, or an access prompt), and no one answers one here. git asks \
                 its next helper or fails.\n  \
                 fix: unlock the keychain, or give that repository a credential helper that \
                 needs no person, then run it again",
                wait.as_secs()
            );
        }
    }
}

fn escalate(
    ended: &mut Vec<(Identity, String)>,
    listing: &str,
    mut start: impl FnMut(i32) -> Option<ProcessStart>,
    mut signal: impl FnMut(i32),
) {
    let present = rows(listing);
    ended.retain(|(process, args)| {
        present
            .iter()
            .any(|row| row.pid == process.pid && row.args == args)
            && process.matches(start(process.pid))
    });
    for (process, _) in ended {
        if process.matches(start(process.pid)) {
            signal(process.pid);
        }
    }
}

fn listing() -> Option<String> {
    aterm_uds::spawnfd::command("/bin/ps")
        .args(["-A", "-o", "pid=,ppid=,args="])
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .stderr(aterm_uds::spawnfd::Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
}

fn end(pid: i32, hard: bool) {
    #[cfg(unix)]
    {
        const SIGKILL: i32 = 9;
        const SIGTERM: i32 = 15;
        unsafe extern "C" {
            fn kill(pid: i32, sig: i32) -> i32;
        }
        // SAFETY: `kill` takes two integers and touches no memory of ours.
        unsafe {
            kill(pid, if hard { SIGKILL } else { SIGTERM });
        }
    }
    #[cfg(not(unix))]
    let _ = (pid, hard);
}

#[cfg(test)]
mod tests {
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

    #[test]
    fn a_helper_is_the_topmost_git_credential_process_under_the_root() {
        assert_eq!(
            helpers(LISTING, 500),
            named(&[(502, "osxkeychain"), (508, "x"), (510, "netrc"), (511, "y")])
        );
        assert_eq!(helpers(LISTING, 600), named(&[(600, "store")]));
        assert!(helpers(LISTING, 999).is_empty());
    }

    #[test]
    fn ending_a_helper_reaches_everything_it_started() {
        assert_eq!(
            subtree(LISTING, 502),
            named(&[
                (502, "git credential-osxkeychain get"),
                (
                    503,
                    "/opt/homebrew/opt/git/libexec/git-core/git-credential-osxkeychain get"
                ),
            ])
        );
        assert_eq!(
            subtree(LISTING, 508),
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
            "503 999 sleep 120\n509 1 sleep 120\n510 1 sleep 120\n",
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
}
