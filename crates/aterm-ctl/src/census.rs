// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE UNREACHED-INSTANCE CENSUS — the aterm windows and headless instances
//! running as this user that no control socket in the rendezvous directory
//! reaches. `aterm doctor` prints it. REPORTED, NEVER STOPPED.
//!
//! WHY IT EXISTS (gap #36, measured 2026-09-26): `./aterm-before --headless`, a
//! test instance whose harness died, had been running for eleven days holding a
//! shell. `aterm ctl ls` never showed it — its socket lived in the scratch
//! directory its harness made — so nothing on the machine said it was there.
//! The lifeline (`aterm_uds::lifeline`) keeps harnesses from leaking the next
//! one; this census is how a person learns of one that already leaked, or of
//! any window no client can reach.
//!
//! WHAT COUNTS as an instance: a process of this user whose program is `aterm` or
//! an `aterm-…` copy of it, started in WINDOW mode — `--headless` or `--window`
//! before any `-e` payload, the `aterm-gui` name, or an app bundle's binary with
//! no arguments — and neither a verb (`aterm ctl …`, `aterm pkg …`) nor a forced
//! `--session`. A bare `aterm` in a terminal is a session: it serves no socket,
//! and is not counted. Everything else in the process table is ignored.
//!
//! WHAT REACHES one: its pid is published in the rendezvous directory — an
//! `aterm-<pid>.sock` there, or a `graph/` entry naming it — the same walk
//! `aterm ctl instances`, `ls` and `windows` make ([`crate::local_instances`]).
//! Publication is the question, not an answer: an instance that is published
//! but does not reply is a different problem that `aterm ctl ls` already names.
//!
//! NEVER KILLED. Whether an unreached instance should end is its owner's call —
//! it may be a person's deliberate `--no-control-sock` window, or a harness run
//! still in progress. The census names each one (pid, age, command line) and
//! stops. When it cannot look — no process table, a rendezvous directory it
//! cannot read — it says it did not measure, never "none".

use std::collections::BTreeSet;

use aterm_types::control_socket::{SocketDirective, socket_directive};

/// One process-table row, as `ps` gives it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcRow {
    /// The process id.
    pub pid: u32,
    /// Its real user id.
    pub uid: u32,
    /// Its age, as `ps`'s POSIX `etime` spells it: `[[dd-]hh:]mm:ss`.
    pub etime: String,
    /// The program: on macOS `ps`'s `comm` is the path it was started by (it may
    /// hold spaces); on Linux it is the kernel's short task name.
    pub program: String,
    /// The whole command line, arguments joined by spaces.
    pub command: String,
}

/// One instance no published socket reaches.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unreached {
    /// The process id — what a person would pass to `kill`, if they decide to.
    pub pid: u32,
    /// How long it has run, in words (`11d 22h`, `3h 05m`, `42s`).
    pub age: String,
    /// Its command line, as `ps` printed it.
    pub command: String,
    /// Started with its control socket switched off (`--no-control-sock`,
    /// `--control-sock 0|off`): unreachable by its own launch, not by accident.
    pub socketless: bool,
}

/// The census: how many instances were counted, and which of them nothing
/// reaches.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Census {
    /// The rendezvous directory that was read, for the words: an instance is
    /// "reached" only relative to it.
    pub dir: String,
    /// Window and headless instances of this user, reached or not.
    pub instances: usize,
    /// The ones no published socket names, oldest first.
    pub unreached: Vec<Unreached>,
}

/// Take the census of this machine, now.
///
/// # Errors
/// The words for a census that was NOT taken: the rendezvous directory could not
/// be resolved or read, or the process table could not be read. Never an empty
/// census standing in for "could not look".
#[cfg(unix)]
pub fn window_census() -> Result<Census, String> {
    let (outcome, targets) = crate::inspect_fleet();
    let (dir, published) = published_in(outcome, targets)?;
    let table = process_table()?;
    census_of(&table, std::process::id(), &published, dir)
}

/// What the rendezvous walk says is PUBLISHED: the directory's words and the
/// pids it names — PURE over [`crate::inspect_fleet`]'s answer.
///
/// A directory that does not EXIST is an answer, not a failure to look: nothing
/// has published there, so every instance is unreached from here. That is the
/// census's own question at its sharpest — a machine whose instances were all
/// started by harnesses with private sockets has no rendezvous directory at all
/// (measured 2026-09-26: `aterm doctor` there said "not measured" beside a
/// running instance it could have named).
///
/// # Errors
/// The directory could not be resolved or could not be read: then nothing is
/// known about what is published, and the census is not taken.
#[cfg(any(unix, test))]
fn published_in(
    outcome: crate::DirOutcome,
    targets: Vec<(u32, String)>,
) -> Result<(String, BTreeSet<u32>), String> {
    let pids = |targets: Vec<(u32, String)>| {
        targets
            .into_iter()
            .map(|(pid, _)| pid)
            .filter(|pid| *pid != 0)
            .collect::<BTreeSet<u32>>()
    };
    match outcome {
        crate::DirOutcome::Found { path, .. } | crate::DirOutcome::Empty(path) => {
            Ok((path.display().to_string(), pids(targets)))
        }
        crate::DirOutcome::Missing { path, .. } => Ok((
            format!("{} (which does not exist)", path.display()),
            BTreeSet::new(),
        )),
        other => Err(crate::discovery_report(other, &[]).0),
    }
}

/// Off Unix there is no `ps` to read; the census says so rather than guessing.
///
/// # Errors
/// Always: the census reads `ps`, so it runs on macOS and Linux only.
#[cfg(not(unix))]
pub fn window_census() -> Result<Census, String> {
    Err("macOS and Linux only".to_string())
}

/// The census over an explicit process table, the caller's own pid (whose row
/// names the user to count for) and the published pids — PURE, so every
/// classification is table-testable.
///
/// # Errors
/// `own_pid` has no row in `table`: without it the census cannot tell whose
/// processes to count, and does not guess.
pub fn census_of(
    table: &[ProcRow],
    own_pid: u32,
    published: &BTreeSet<u32>,
    dir: String,
) -> Result<Census, String> {
    let uid = table
        .iter()
        .find(|row| row.pid == own_pid)
        .map(|row| row.uid)
        .ok_or_else(|| "this process is missing from the process table".to_string())?;
    let mut instances = 0;
    let mut unreached: Vec<(u64, Unreached)> = Vec::new();
    for row in table.iter().filter(|row| row.uid == uid) {
        let Some(args) = window_args(row) else {
            continue;
        };
        instances += 1;
        if published.contains(&row.pid) {
            continue;
        }
        let secs = etime_secs(&row.etime);
        unreached.push((
            secs.unwrap_or(0),
            Unreached {
                pid: row.pid,
                age: secs.map_or_else(|| row.etime.clone(), age_words),
                command: row.command.clone(),
                socketless: socket_off(&args),
            },
        ));
    }
    unreached.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.pid.cmp(&b.1.pid)));
    Ok(Census {
        dir,
        instances,
        unreached: unreached.into_iter().map(|(_, u)| u).collect(),
    })
}

/// The arguments of `row` when it is a window-mode aterm instance, else `None`.
fn window_args(row: &ProcRow) -> Option<Vec<&str>> {
    let base = row.program.rsplit('/').next().unwrap_or(&row.program);
    if base != "aterm" && !base.starts_with("aterm-") {
        return None;
    }
    // The arguments: what follows the program on the command line. On macOS the
    // command line starts with `comm` verbatim (spaces included); elsewhere
    // `comm` is a short name, and the first word of the command line is argv[0].
    let rest = match row.command.strip_prefix(row.program.as_str()) {
        Some(r) if r.is_empty() || r.starts_with(char::is_whitespace) => r,
        _ => row
            .command
            .split_once(char::is_whitespace)
            .map_or("", |(_, r)| r),
    };
    // Everything after `-e`/`--command`/`--` is a child command's argv, not ours.
    let args: Vec<&str> = rest
        .split_whitespace()
        .take_while(|a| !matches!(*a, "-e" | "--command" | "--"))
        .collect();
    let in_bundle = row.program.contains(".app/Contents/MacOS/");
    let window = base == "aterm-gui"
        || args.iter().any(|a| matches!(*a, "--headless" | "--window"))
        || (in_bundle && args.is_empty());
    // A verb (`aterm ctl …`) and a forced session take no window, whatever else
    // the line says.
    let verb = args.first().is_some_and(|a| !a.starts_with('-'));
    let session = args.contains(&"--session");
    (window && !verb && !session).then_some(args)
}

/// Whether `args` switched the control socket off — the same decision the
/// instance itself made ([`socket_directive`]).
fn socket_off(args: &[&str]) -> bool {
    let value = args
        .iter()
        .position(|a| *a == "--control-sock")
        .and_then(|i| args.get(i + 1).copied());
    let off = args.contains(&"--no-control-sock").then_some("1");
    socket_directive(value, off) == SocketDirective::Disabled
}

/// `ps`'s `etime` (`[[dd-]hh:]mm:ss`) in seconds; `None` for anything else.
fn etime_secs(etime: &str) -> Option<u64> {
    let (days, clock) = match etime.trim().split_once('-') {
        Some((d, rest)) => (d.parse::<u64>().ok()?, rest),
        None => (0, etime.trim()),
    };
    let mut parts = clock.split(':').rev();
    let secs: u64 = parts.next()?.parse().ok()?;
    let mins: u64 = parts.next().unwrap_or("0").parse().ok()?;
    let hours: u64 = parts.next().unwrap_or("0").parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some(days * 86_400 + hours * 3_600 + mins * 60 + secs)
}

/// An age in the two largest units: `11d 22h`, `3h 05m`, `4m 12s`, `42s`.
fn age_words(secs: u64) -> String {
    let (d, h, m, s) = (secs / 86_400, secs / 3_600 % 24, secs / 60 % 60, secs % 60);
    if d > 0 {
        format!("{d}d {h}h")
    } else if h > 0 {
        format!("{h}h {m:02}m")
    } else if m > 0 {
        format!("{m}m {s:02}s")
    } else {
        format!("{s}s")
    }
}

/// The process table, from two `ps` reads joined by pid: the program column can
/// hold spaces and so can the command line, so each has to be the LAST column
/// of its own read. A process born or gone between the reads is simply not in
/// both, and is skipped.
///
/// # Errors
/// `ps` could not be run, or printed nothing a row could be read from.
#[cfg(unix)]
fn process_table() -> Result<Vec<ProcRow>, String> {
    let identity = ps(&["-o", "pid=", "-o", "uid=", "-o", "etime=", "-o", "comm="])?;
    let commands = ps(&["-o", "pid=", "-o", "command="])?;
    let rows = join_ps(&identity, &commands);
    if rows.is_empty() {
        return Err("the process table read back empty".to_string());
    }
    Ok(rows)
}

/// One `ps -A -ww` read with the given columns, as text.
#[cfg(unix)]
fn ps(columns: &[&str]) -> Result<String, String> {
    // By path first: a doctor run under a scrubbed PATH still finds it.
    let program = ["/bin/ps", "/usr/bin/ps"]
        .into_iter()
        .find(|p| std::path::Path::new(p).exists())
        .unwrap_or("ps");
    let out = std::process::Command::new(program)
        .args(["-A", "-ww"])
        .args(columns)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("the process table could not be read (`ps`: {e})"))?;
    if !out.status.success() {
        return Err(format!(
            "the process table could not be read (`ps` exited {})",
            out.status
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Join the two `ps` reads ([`process_table`]) into rows, PURE.
#[cfg(any(unix, test))]
fn join_ps(identity: &str, commands: &str) -> Vec<ProcRow> {
    /// The first whitespace-separated word and the rest, trimmed at the front.
    fn word(s: &str) -> Option<(&str, &str)> {
        let s = s.trim_start();
        let end = s.find(char::is_whitespace)?;
        Some((&s[..end], s[end..].trim_start()))
    }
    let command_of: std::collections::HashMap<u32, &str> = commands
        .lines()
        .filter_map(|line| {
            let (pid, rest) = word(line)?;
            Some((pid.parse().ok()?, rest.trim_end()))
        })
        .collect();
    identity
        .lines()
        .filter_map(|line| {
            let (pid, rest) = word(line)?;
            let pid: u32 = pid.parse().ok()?;
            let (uid, rest) = word(rest)?;
            let (etime, program) = word(rest)?;
            Some(ProcRow {
                pid,
                uid: uid.parse().ok()?,
                etime: etime.to_string(),
                program: program.trim_end().to_string(),
                command: (*command_of.get(&pid)?).to_string(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(pid: u32, uid: u32, etime: &str, program: &str, command: &str) -> ProcRow {
        ProcRow {
            pid,
            uid,
            etime: etime.to_string(),
            program: program.to_string(),
            command: command.to_string(),
        }
    }

    /// The table this machine had on 2026-09-26, reduced: the owner's app, the
    /// eleven-day orphan, a test binary whose name starts with `aterm`, a session,
    /// a client verb, another user's instance — and the doctor itself.
    fn table() -> Vec<ProcRow> {
        vec![
            row(
                100,
                501,
                "01:00",
                "/opt/aterm/bin/aterm",
                "/opt/aterm/bin/aterm doctor",
            ),
            row(
                7641,
                501,
                "01-11:48:49",
                "/Applications/aterm.app/Contents/MacOS/aterm",
                "/Applications/aterm.app/Contents/MacOS/aterm --window",
            ),
            row(
                67548,
                501,
                "11-22:32:03",
                "./aterm-before",
                "./aterm-before --headless",
            ),
            row(
                19235,
                501,
                "03-22:46:46",
                "/w/target/debug/deps/aterm_pty-8a8f4252995814f9",
                "/w/target/debug/deps/aterm_pty-8a8f4252995814f9 --headless",
            ),
            row(300, 501, "05:00", "aterm", "aterm"),
            row(301, 501, "00:02", "aterm", "aterm ctl --sock /x windows"),
            row(302, 501, "00:09", "aterm", "aterm --session --headless"),
            row(400, 502, "09:00", "aterm", "aterm --headless"),
        ]
    }

    /// THE CENSUS: the published window is reached, the eleven-day orphan is
    /// named with its age and command line, and nothing else is counted — not
    /// the test binary, the session, the client verb, the forced session, the
    /// other user's instance, or the doctor.
    #[test]
    fn it_names_the_unreached_instance_and_ignores_the_reached_one() {
        let published = BTreeSet::from([7641]);
        let census = census_of(&table(), 100, &published, "/run/aterm".into()).expect("census");
        assert_eq!(census.instances, 2, "{census:?}");
        assert_eq!(
            census.unreached,
            vec![Unreached {
                pid: 67548,
                age: "11d 22h".to_string(),
                command: "./aterm-before --headless".to_string(),
                socketless: false,
            }]
        );
        // Publish the orphan too and the census is clean — the count is unchanged.
        let both = BTreeSet::from([7641, 67548]);
        let census = census_of(&table(), 100, &both, "/run/aterm".into()).expect("census");
        assert_eq!((census.instances, census.unreached.len()), (2, 0));
    }

    /// Nothing is guessed: without its own row the census cannot say whose
    /// processes to count, and says so.
    #[test]
    fn a_census_without_its_own_row_is_not_taken() {
        let err =
            census_of(&table(), 99_999, &BTreeSet::new(), String::new()).expect_err("no own row");
        assert!(err.contains("missing from the process table"), "{err}");
    }

    /// Window mode is read the way the front door reads it: before the `-e`
    /// payload only, a verb or `--session` wins, the `aterm-gui` name and a bare
    /// bundle launch are windows, and a socket switched off is said so.
    #[test]
    fn window_mode_is_read_from_the_launch_flags() {
        let is_window = |program: &str, command: &str| {
            window_args(&row(1, 1, "00:01", program, command)).is_some()
        };
        assert!(is_window("aterm", "aterm --headless --columns 80"));
        assert!(is_window("aterm-gui", "aterm-gui"));
        assert!(is_window(
            "/Applications/aterm.app/Contents/MacOS/aterm",
            "/Applications/aterm.app/Contents/MacOS/aterm"
        ));
        assert!(!is_window("aterm", "aterm -e sh -c 'aterm --headless'"));
        assert!(!is_window("aterm", "aterm -- --window"));
        assert!(!is_window("aterm", "aterm pkg doctor --window"));
        assert!(!is_window("aterm-link", "aterm-link serve --fleet f"));
        assert!(!is_window("zsh", "zsh -c aterm --headless"));
        // A program path with spaces (macOS `comm`): the arguments start after it.
        assert!(is_window(
            "/Users//a/Library/Application Support/aterm/bin/aterm",
            "/Users//a/Library/Application Support/aterm/bin/aterm --headless"
        ));
        // Linux: `comm` is the short task name; argv[0] is the command's first word.
        assert!(is_window("aterm", "/usr/local/bin/aterm --headless"));

        let socketless = |command: &str| {
            let r = row(1, 1, "00:01", "aterm", command);
            socket_off(&window_args(&r).expect("a window"))
        };
        assert!(socketless("aterm --headless --no-control-sock"));
        assert!(socketless("aterm --headless --control-sock off"));
        assert!(socketless("aterm --headless --control-sock 0"));
        assert!(!socketless("aterm --headless --control-sock /tmp/a.sock"));
        assert!(!socketless("aterm --headless"));
    }

    /// `ps`'s two reads join by pid; a row in only one read is skipped, and a
    /// program with spaces keeps them.
    #[test]
    fn the_two_ps_reads_join_by_pid() {
        let identity = "  7641   501 01-11:48:49 /Applications/aterm.app/Contents/MacOS/aterm\n\
                        72325   501       19:30 /Users//a/Library/Application Support/x/targo\n\
                           12   501       00:01 gone\n";
        let commands = " 7641 /Applications/aterm.app/Contents/MacOS/aterm --window\n\
                        72325 /Users//a/Library/Application Support/x/targo --unverified test\n";
        let rows = join_ps(identity, commands);
        assert_eq!(rows.len(), 2, "{rows:?}");
        assert_eq!(rows[0].pid, 7641);
        assert_eq!(rows[0].etime, "01-11:48:49");
        assert_eq!(
            rows[0].program,
            "/Applications/aterm.app/Contents/MacOS/aterm"
        );
        assert_eq!(
            rows[1].program,
            "/Users//a/Library/Application Support/x/targo"
        );
        assert_eq!(
            rows[1].command,
            "/Users//a/Library/Application Support/x/targo --unverified test"
        );
    }

    /// What is published, per rendezvous answer: a directory's pids (a legacy
    /// graph entry's placeholder 0 is no pid); a directory that does not exist
    /// publishes nothing, so every instance is unreached — counted, not "not
    /// measured"; one that cannot be resolved or read is not a census at all.
    #[test]
    fn a_missing_directory_publishes_nothing_and_an_unreadable_one_is_not_measured() {
        use std::path::PathBuf;
        let targets = vec![
            (7641, "/r/aterm-7641.sock".to_string()),
            (0, "/x".to_string()),
        ];
        let found = crate::DirOutcome::Found {
            path: PathBuf::from("/r"),
            n: 2,
        };
        let (dir, published) = published_in(found, targets).expect("found");
        assert_eq!((dir.as_str(), published), ("/r", BTreeSet::from([7641])));

        let missing = crate::DirOutcome::Missing {
            path: PathBuf::from("/scratch/run/aterm"),
            why: "resolved from $XDG_RUNTIME_DIR".to_string(),
        };
        let (dir, published) = published_in(missing, Vec::new()).expect("missing is an answer");
        assert_eq!(dir, "/scratch/run/aterm (which does not exist)");
        assert!(published.is_empty());
        let census = census_of(&table(), 100, &published, dir).expect("census");
        let pids: Vec<u32> = census.unreached.iter().map(|u| u.pid).collect();
        assert_eq!(
            pids,
            [67548, 7641],
            "every instance is unreached from there"
        );

        let unreadable = crate::DirOutcome::Unreadable {
            path: PathBuf::from("/r"),
            err: std::io::Error::from(std::io::ErrorKind::PermissionDenied),
        };
        let err = published_in(unreadable, Vec::new()).expect_err("not measured");
        assert!(err.contains("cannot be read"), "{err}");
        let err =
            published_in(crate::DirOutcome::Unresolvable, Vec::new()).expect_err("not measured");
        assert!(err.contains("cannot resolve"), "{err}");
    }

    /// Ages read the way a person reads them, oldest first in the census.
    #[test]
    fn ages_are_words_and_the_oldest_comes_first() {
        assert_eq!(
            etime_secs("11-22:32:03"),
            Some(11 * 86_400 + 22 * 3_600 + 32 * 60 + 3)
        );
        assert_eq!(etime_secs("19:30"), Some(19 * 60 + 30));
        assert_eq!(etime_secs("1:02:03"), Some(3_723));
        assert_eq!(etime_secs("bad"), None);
        assert_eq!(age_words(11 * 86_400 + 22 * 3_600 + 5), "11d 22h");
        assert_eq!(age_words(3 * 3_600 + 5 * 60), "3h 05m");
        assert_eq!(age_words(4 * 60 + 12), "4m 12s");
        assert_eq!(age_words(42), "42s");

        let mut t = table();
        t.push(row(
            68000,
            501,
            "00:05",
            "aterm",
            "aterm --headless --no-control-sock",
        ));
        let census = census_of(&t, 100, &BTreeSet::from([7641]), String::new()).expect("census");
        let pids: Vec<u32> = census.unreached.iter().map(|u| u.pid).collect();
        assert_eq!(pids, [67548, 68000]);
        assert!(census.unreached[1].socketless);
    }
}
