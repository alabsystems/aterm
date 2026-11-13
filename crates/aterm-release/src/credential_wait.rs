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

/// One process of the watch's [`table`].
#[derive(Debug, PartialEq, Eq)]
struct Row {
    pid: i32,
    ppid: i32,
    /// Its argument words joined by spaces; empty where they were not read.
    args: String,
}

/// The process table one look reads, in this process: every process's pid and parent
/// ([`crate::interrupt::listing`]), and the arguments ([`args_of`]) of only the processes below
/// `root` and those in `also`. No program runs and no other process's arguments are read: a
/// `ps` that reads every process's hung by the thousand in a program-check wedge
/// (`docs/DESIGN-leftover-processes-2026-10-16.md` M29). `None` when the table cannot be read.
fn table(root: i32, also: &[i32]) -> Option<Vec<Row>> {
    let procs = crate::interrupt::listing()?;
    Some(table_of(
        procs.into_iter().map(|p| (p.pid, p.parent)),
        root,
        also,
        args_of,
    ))
}

/// [`table`] over `procs`' `(pid, parent)` pairs, reading the arguments of the rows below
/// `root` and in `also` with `read`, and of no other row.
fn table_of(
    procs: impl IntoIterator<Item = (i32, i32)>,
    root: i32,
    also: &[i32],
    mut read: impl FnMut(i32) -> String,
) -> Vec<Row> {
    let mut rows: Vec<Row> = procs
        .into_iter()
        .map(|(pid, ppid)| Row {
            pid,
            ppid,
            args: String::new(),
        })
        .collect();
    let tree = below(&rows, root);
    for row in &mut rows {
        if tree.contains(&row.pid) || also.contains(&row.pid) {
            row.args = read(row.pid);
        }
    }
    rows
}

/// `pid`'s argument words joined by spaces, read in this process only once its one thread has
/// run ([`aterm_uds::process::thread_ran`]: a program macOS's program check still holds can hang
/// the read) and only while the same process holds the pid across the read
/// ([`aterm_uds::process::read_process_argv`]). Empty when unread: another user's process, one
/// not yet started, one gone, a zombie. No read is atomic with an exec: one into a held program
/// after the check can still hang the argument read that follows, which nothing here bounds
/// (§6.4 of `docs/DESIGN-leftover-processes-2026-10-16.md`).
fn args_of(pid: i32) -> String {
    if aterm_uds::process::thread_ran(pid) != Some(true) {
        return String::new();
    }
    aterm_uds::process::read_process_argv(pid).map_or_else(String::new, |words| {
        let cap: usize = words
            .iter()
            .map(|w| w.len().saturating_add(1))
            .sum::<usize>()
            .saturating_sub(1);
        let mut out = String::with_capacity(cap);
        for (i, word) in words.iter().enumerate() {
            if i > 0 {
                out.push(' ');
            }
            out.push_str(&word.to_string_lossy());
        }
        out
    })
}

fn below(rows: &[Row], top: i32) -> BTreeSet<i32> {
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

/// The git credential helpers running under `root` in `rows`.
#[must_use]
fn helpers(rows: &[Row], root: i32) -> Vec<(i32, String)> {
    let tree = below(rows, root);
    let named: Vec<(&Row, &str)> = rows
        .iter()
        .filter(|row| tree.contains(&row.pid))
        .filter_map(|row| helper_name(&row.args).map(|name| (row, name)))
        .collect();
    let helper_pids: BTreeSet<i32> = named.iter().map(|(row, _)| row.pid).collect();
    let parents: std::collections::BTreeMap<i32, i32> =
        rows.iter().map(|row| (row.pid, row.ppid)).collect();
    let under_a_helper = |row: &Row| {
        let mut at = row.ppid;
        for _ in 0..rows.len() {
            if helper_pids.contains(&at) {
                return true;
            }
            match parents.get(&at).copied() {
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

/// `top` and every process below it in `rows`.
#[must_use]
fn subtree(rows: &[Row], top: i32) -> Vec<(i32, String)> {
    let tree = below(rows, top);
    rows.iter()
        .filter(|row| tree.contains(&row.pid))
        .map(|row| (row.pid, row.args.clone()))
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
        || program
            .get(..6)
            .is_some_and(|p| p.eq_ignore_ascii_case("python"))
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
        let also: Vec<i32> = ended.iter().map(|(process, _)| process.pid).collect();
        let Some(rows) = table(root.pid, &also) else {
            continue;
        };
        escalate(&mut ended, &rows, read_process_start, |pid| end(pid, true));
        if !root.current() {
            continue;
        }
        let running = helpers(&rows, root.pid)
            .into_iter()
            .filter_map(|(pid, name)| Identity::read(pid).map(|process| (process, name)))
            .collect();
        for (helper, name) in seen.overdue(running, Instant::now(), wait) {
            if ended.iter().any(|(p, _)| *p == helper) {
                continue;
            }
            let reached: Vec<_> = subtree(&rows, helper.pid)
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

/// Keeps each of `ended` that is still the process it was (its pid in `present` with the same
/// arguments, and the same start) and signals each kept one. One whose arguments were unread when
/// it was ended (its thread had not run: [`args_of`]) takes the words `present` reads for it now,
/// its start alone naming it, so a process that ignores the first signal is still killed once it
/// runs.
fn escalate(
    ended: &mut Vec<(Identity, String)>,
    present: &[Row],
    mut start: impl FnMut(i32) -> Option<ProcessStart>,
    mut signal: impl FnMut(i32),
) {
    ended.retain_mut(|(process, args)| {
        let Some(row) = present.iter().find(|row| row.pid == process.pid) else {
            return false;
        };
        if args.is_empty() {
            args.clone_from(&row.args);
        }
        row.args == *args && process.matches(start(process.pid))
    });
    for (process, _) in ended {
        if process.matches(start(process.pid)) {
            signal(process.pid);
        }
    }
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
#[path = "credential_wait_tests.rs"]
mod tests;
