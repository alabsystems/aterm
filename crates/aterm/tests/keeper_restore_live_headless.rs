// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE EXACT SCREEN AFTER A CRASH, on real processes (the crash-restore plan §7.3's live lane).

#![allow(
    clippy::disallowed_types,
    reason = "test code: a stand-in child may inherit descriptors"
)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use aterm_update::recovery_ledger::{KeeperAtLaunch, Row, ledger_path, read_rows};

use crate::live_ctl::{Ctl, fields};
use crate::{child_output, headless_boot, launch_isolation};

/// A wait for something that must happen is a hang detector (AGENTS.md).
const HANG: Duration = aterm_spec::wait::MUST_HAPPEN;
/// How long macOS may hold the first exec of a new program: the bound on the keeper showing
/// it runs from its fresh copy of its code ([`keeper_ran`]).
const FIRST_EXEC_HANG: Duration = aterm_uds::deadline::FIRST_EXEC_HANG;
/// How often a wait on a status line asks again.
const GAP: Duration = Duration::from_millis(50);

/// The window's journal off: the negative control's seam (`keeper_link::journal_refusal`).
const NO_JOURNAL_SEAM: &str = "ATERM_DEBUG_NO_CRASH_JOURNAL";

/// The window's keyframes held after the first, so a crash leaves a tail to replay
/// (`crash_restore::HOLD_KEYFRAMES_SEAM`).
const HOLD_KEYFRAMES_SEAM: &str = "ATERM_DEBUG_CRASH_JOURNAL_HOLD_KEYFRAMES";

/// The recovery census's seam (`recovery_census::LEDGER_SEAM`): every instance writes the
/// row the installed app's cold start would into the world's own ledger, so the test reads
/// the `keeper=` and `restored=` words the soak judges (`aterm_update::restore_soak`).
const LEDGER_SEAM: &str = "ATERM_DEBUG_RECOVERY_LEDGER";

/// One scratch world the keeper and both instances share: one `$HOME`, one config with
/// the keeper on, one `$XDG_RUNTIME_DIR` (where the keeper's socket lives, `/tmp` being no
/// private `$TMPDIR`), in a directory made for it ([`world`]); removed on drop, after the
/// keeper and every instance (declared after it, so dropped first), past the hung-up shell's
/// last history write, unless it holds a copy of the keeper's code that never showed it ran
/// ([`unrun_copies`]).
struct World(PathBuf);

impl Drop for World {
    fn drop(&mut self) {
        let unrun = unrun_copies(&self.0);
        if unrun.is_empty() {
            headless_boot::remove_world(&self.0);
        } else {
            eprintln!(
                "KEPT {}: the keeper's copy {unrun:?} never showed it ran, so macOS may still be \
                 assessing its first exec; remove it once no process runs from it",
                self.0.display()
            );
        }
    }
}

/// Where the world's keepers keep the copies of their code they run from.
fn keeper_code(root: &Path) -> PathBuf {
    aterm_keeper::code::code_dir_in(&root.join("home"))
}

/// The copies of the keeper's code in the world that never showed they ran (no
/// [`aterm_keeper::code::RAN`] mark): macOS may still be assessing the first exec of one, and
/// a file unlinked mid-assessment can strand every exec waiting on macOS's program check, so
/// it stays until a child showed it ran from it (AGENTS.md), however old its exec stamp is.
/// A half-made `.partial` copy was never run, and an entry no keeper named is no copy.
fn unrun_copies(root: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(keeper_code(root))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            !name.ends_with(".partial") && aterm_keeper::code::owner_of(&name).is_some()
        })
        .map(|entry| entry.path())
        .filter(|copy| !copy.join(aterm_keeper::code::RAN).exists())
        .collect()
}

/// One child process, SIGKILLed and reaped on drop.
struct Proc {
    child: Child,
    _lifeline: Option<aterm_uds::lifeline::Lifeline>,
}

impl Drop for Proc {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A new world in a directory made for it (`/tmp/atkr-<tag>-…`), never one an earlier run
/// kept ([`World`]) and never removed to make room.
fn world(tag: &str) -> World {
    let made = aterm_tempfile::Builder::new()
        .prefix(&format!("atkr-{tag}-"))
        .tempdir_in("/tmp");
    let world = match made {
        Ok(dir) => World(dir.keep()),
        Err(e) => aterm_spec::host::could_not_run(format!("no scratch world in /tmp ({e})")),
    };
    let root = &world.0;
    if !launch_isolation::sock_fits(&root.join("run/aterm/keeper-dev-00000000.sock"))
        || launch_isolation::prepare(root).is_err()
    {
        aterm_spec::host::could_not_run("no scratch world with a socket path short enough");
    }
    let config = root.join("cfg/aterm/aterm.toml");
    let mut text = std::fs::read_to_string(&config).expect("the scratch config");
    text.push_str("[keeper]\nenabled = true\n");
    std::fs::write(&config, text).expect("turn the keeper on");
    world
}

/// `aterm keeper <args…>` in the world's environment, read to its end.
fn keeper_cli(root: &Path, args: &[&str]) -> String {
    let mut cmd = Command::new(launch_isolation::aterm_executable());
    launch_isolation::apply(&mut cmd, root);
    cmd.arg("keeper")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = cmd.spawn().expect("spawn aterm keeper");
    let out = child_output::wait_and_read(child, std::time::Instant::now() + HANG * 2)
        .unwrap_or_else(|| panic!("aterm keeper {args:?} did not exit in time"));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The keeper's `status` text, `key=value` words across its lines.
fn keeper_status(root: &Path) -> String {
    keeper_cli(root, &["status"])
}

/// Wait until the keeper's status holds `word`.
#[track_caller]
fn await_keeper(root: &Path, word: &str) {
    let mut last = String::new();
    let seen = aterm_spec::wait::within(HANG, GAP, || {
        last = keeper_status(root);
        last.split_whitespace().any(|w| w == word).then_some(())
    });
    assert!(
        seen.is_some(),
        "the keeper never said `{word}`; its status:\n{last}"
    );
}

/// This build's keeper, in the foreground (`serve --no-relaunch`: no window is brought
/// back, the next launch recovers). It runs again from a fresh copy of its code
/// (`aterm_keeper::code`), a new program whose first exec macOS holds for as long as its
/// queue of new programs makes it (over a minute behind a build's), so the one-minute wait for
/// its answer starts once it showed it runs ([`await_start`]).
fn keeper(root: &Path) -> Proc {
    let log = root.join("keeper.log");
    let file = std::fs::File::create(&log).expect("the keeper's log");
    let mut cmd = Command::new(launch_isolation::aterm_executable());
    launch_isolation::apply(&mut cmd, root);
    cmd.args(["keeper", "serve", "--no-relaunch"])
        .stdin(Stdio::null())
        .stdout(file.try_clone().expect("log"))
        .stderr(file);
    let child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => aterm_spec::host::could_not_run(format!("cannot launch the keeper ({e})")),
    };
    let mut keeper = Proc {
        child,
        _lifeline: None,
    };
    let pid = keeper.child.id();
    let started = await_start(root, pid, HANG, FIRST_EXEC_HANG, &mut || {
        !matches!(keeper.child.try_wait(), Ok(None))
    });
    match started {
        Start::Ran | Start::Ended => {}
        Start::NoCopy => {
            aterm_uds::deadline::require_trust_service();
            panic!(
                "the keeper (pid {pid}) made no copy of its code to run from in {HANG:?}: no \
                 exec stamp in one, nothing in its log"
            );
        }
        Start::NotRun => {
            let held = aterm_uds::process::at_program_check(i32::try_from(pid).unwrap_or(0));
            if let Some(words) = aterm_uds::deadline::held_words(pid, FIRST_EXEC_HANG, held) {
                aterm_spec::host::could_not_run(words);
            }
            panic!(
                "the keeper (pid {pid}) never showed it runs: no mark in its copy of its code, \
                 nothing in its log"
            );
        }
    }
    if let Ok(Some(status)) = keeper.child.try_wait() {
        panic!(
            "the keeper exited ({status}) before it answered; its log:\n{}",
            headless_boot::log_tail(&log)
        );
    }
    let mut last = String::new();
    let answered = aterm_spec::wait::within(HANG, GAP, || {
        last = keeper_status(root);
        last.lines()
            .any(|l| l.starts_with("masters="))
            .then_some(())
    });
    assert!(
        answered.is_some(),
        "the keeper never answered; its status:\n{last}\nits log:\n{}",
        headless_boot::log_tail(&log)
    );
    keeper
}

/// How far a keeper's start got ([`await_start`]).
#[derive(Debug, PartialEq, Eq)]
enum Start {
    /// It showed it runs.
    Ran,
    /// It exited.
    Ended,
    /// No copy of its code was stamped for its exec, nor did it write a line.
    NoCopy,
    /// Its copy was stamped for its exec and never showed it runs.
    NotRun,
}

/// Wait for keeper `pid` to show it runs, `ended` saying whether it exited, in two waits. First
/// up to `copy_hang` for its copy's exec stamp ([`aterm_uds::deadline::EXEC_STAMP`], written
/// just before it runs again from the copy): the work before is its own, its first exec (this
/// build's) warmed by [`launch_isolation::apply`]. Then up to `run_hang` for the copy's mark
/// ([`aterm_keeper::code::RAN`]): that wait is macOS's first-exec check of the copy. A line
/// in its log answers either wait ([`keeper_reached`]).
fn await_start(
    root: &Path,
    pid: u32,
    copy_hang: Duration,
    run_hang: Duration,
    ended: &mut dyn FnMut() -> bool,
) -> Start {
    let mut wait = |hang, mark| {
        aterm_spec::wait::within(hang, GAP, || {
            if ended() {
                Some(Start::Ended)
            } else {
                keeper_reached(root, pid, mark).then_some(Start::Ran)
            }
        })
    };
    match wait(copy_hang, aterm_uds::deadline::EXEC_STAMP) {
        None => Start::NoCopy,
        Some(Start::Ran) => wait(run_hang, aterm_keeper::code::RAN).unwrap_or(Start::NotRun),
        Some(other) => other,
    }
}

/// Whether keeper `pid`'s copy of its code in the world ([`keeper_code`], `<pid>-<start>`,
/// never a half-made `.partial` one) holds the file `mark`, or the keeper wrote to its log,
/// which before it runs from its copy it does only where its start went another way: it
/// exits, serves from this build, or could not stamp its copy (a world made for it holds no
/// earlier keeper's copy for its sweep to speak of).
fn keeper_reached(root: &Path, pid: u32, mark: &str) -> bool {
    let said = std::fs::metadata(root.join("keeper.log")).is_ok_and(|meta| meta.len() > 0);
    said || std::fs::read_dir(keeper_code(root))
        .into_iter()
        .flatten()
        .flatten()
        .any(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            !name.ends_with(".partial")
                && aterm_keeper::code::owner_of(&name).is_some_and(|(owner, _)| owner == pid)
                && entry.path().join(mark).exists()
        })
}

/// One headless instance on its own socket in the world, hearing no system's memory pressure
/// ([`launch_isolation::SYSTEM_PRESSURE`]), with each debug seam in `seams` set to its value.
fn boot(root: &Path, tag: &str, seams: &[(&str, &str)]) -> (Proc, String) {
    let sock = root.join(format!("run/aterm/{tag}.sock"));
    let log_path = root.join(format!("{tag}.log"));
    let (out, err) = match std::fs::File::create(&log_path).and_then(|f| Ok((f.try_clone()?, f))) {
        Ok(pair) => pair,
        Err(e) => aterm_spec::host::could_not_run(format!("cannot open the instance log ({e})")),
    };
    let mut cmd = Command::new(launch_isolation::aterm_executable());
    launch_isolation::apply(&mut cmd, root);
    cmd.args(["--headless", launch_isolation::NO_REROUTE])
        .arg("--control-sock")
        .arg(&sock)
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err);
    for (seam, value) in seams {
        cmd.env(seam, value);
    }
    let ledger = ledger_dir(root);
    if let Err(e) = std::fs::create_dir_all(&ledger) {
        aterm_spec::host::could_not_run(format!("cannot make the census's folder ({e})"));
    }
    cmd.env(LEDGER_SEAM, &ledger);
    let lifeline = launch_isolation::lifeline(&mut cmd, root);
    let mut ready = headless_boot::arm_ready(&mut cmd, root);
    let child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => aterm_spec::host::could_not_run(format!("cannot launch aterm --headless ({e})")),
    };
    let mut instance = Proc {
        child,
        _lifeline: Some(lifeline),
    };
    headless_boot::await_ready(
        &mut instance,
        |p| &mut p.child,
        &mut ready,
        &log_path,
        |_| true,
    );
    (instance, sock.to_string_lossy().into_owned())
}

/// The world's recovery ledger folder, every instance's census row in it.
fn ledger_dir(root: &Path) -> PathBuf {
    root.join("ledger")
}

/// The census row instance `pid`'s cold start wrote (its census thread's, awaited under the
/// hang detector).
#[track_caller]
fn census_row(root: &Path, pid: u32) -> Row {
    let path = ledger_path(&ledger_dir(root));
    let mut last = Vec::new();
    let row = aterm_spec::wait::within(HANG, GAP, || {
        last = read_rows(&path);
        last.iter().find(|row| row.pid == pid).cloned()
    });
    row.unwrap_or_else(|| panic!("pid {pid} wrote no census row; the ledger: {last:#?}"))
}

/// Wait until the instance's session `status` has `key=value`; the last line read.
#[track_caller]
fn await_status(ctl: &mut Ctl, key: &str, value: &str) -> String {
    let mut last = String::new();
    let seen = aterm_spec::wait::within(HANG, GAP, || {
        last = ctl.status();
        (fields(&last).get(key) == Some(&value)).then_some(())
    });
    assert!(seen.is_some(), "`{key}={value}` never held; status: {last}");
    last
}

/// What a restore must bring back: the screen text and every mode.
fn screen_and_modes(ctl: &mut Ctl) -> (Vec<String>, BTreeMap<String, String>) {
    (ctl.text(), ctl.modes())
}

/// The absolute row of the one line holding `marker`, and of the live screen's top
/// (`search`'s `screen_first=`), in the numbering `line <row>` reads.
fn located(ctl: &mut Ctl, marker: &str) -> (u64, u64) {
    let (header, hits) = ctl.counted(&format!("search {marker}"));
    assert_eq!(
        hits.len(),
        1,
        "`{marker}` is on one line: {header} {hits:?}"
    );
    let row = hits[0]
        .split_whitespace()
        .next()
        .and_then(|row| row.parse().ok())
        .unwrap_or_else(|| panic!("a row in {hits:?}"));
    let first = fields(&header)
        .get("screen_first")
        .and_then(|row| row.parse().ok())
        .unwrap_or_else(|| panic!("screen_first= in {header}"));
    (row, first)
}

/// The scrollback's line count (`lines`).
fn scrollback_lines(ctl: &mut Ctl) -> u64 {
    ctl.ok("lines")
        .split_whitespace()
        .nth(1)
        .and_then(|n| n.parse().ok())
        .expect("`lines` counts")
}

/// Wait until the scrollback holds `lines` lines: a recovered session's history import
/// runs on a worker after the recovery, so its lines arrive after the screen.
#[track_caller]
fn await_scrollback(ctl: &mut Ctl, lines: u64) {
    let mut last = 0;
    let seen = aterm_spec::wait::within(HANG, GAP, || {
        last = scrollback_lines(ctl);
        (last == lines).then_some(())
    });
    assert!(
        seen.is_some(),
        "the scrollback never held {lines} lines; it holds {last}"
    );
}

/// Every scrollback line above the screen's first row `first`, by absolute row (`line
/// <row>`), `lines` of them.
fn history(ctl: &mut Ctl, first: u64, lines: u64) -> BTreeMap<u64, String> {
    (first - lines..first)
        .map(|row| (row, ctl.line(&format!("line {row}"))))
        .collect()
}

/// Whether `row` is the shell's prompt: the scratch world's `/bin/sh` draws `…$ `
/// (`#` as root).
fn is_prompt(row: &str) -> bool {
    row.trim_end().ends_with(['$', '#'])
}

/// Wait until the shell reads at a prompt: the last non-blank row below the last row
/// holding `after` (anywhere, with `None`) is a prompt.
#[track_caller]
fn await_prompt(ctl: &mut Ctl, after: Option<&str>) {
    let mut last = Vec::new();
    let seen = aterm_spec::wait::within(HANG, GAP, || {
        last = ctl.text();
        let from = match after {
            Some(marker) => last.iter().rposition(|row| row.contains(marker))? + 1,
            None => 0,
        };
        last[from..]
            .iter()
            .rfind(|row| !row.trim().is_empty())
            .filter(|row| is_prompt(row))
            .map(|_| ())
    });
    assert!(
        seen.is_some(),
        "the shell never drew its prompt (below {after:?}); the screen: {last:?}"
    );
}

/// A's session: the output, then the wait for its journal to have caught up and its region
/// to be in the keeper's custody (or, under the seam, the keeper holding its claim with no
/// region).
fn run_a(root: &Path, ctl: &mut Ctl, seam: bool) -> (Vec<String>, BTreeMap<String, String>) {
    if seam {
        await_status(ctl, "journal_off", "seam");
    } else {
        await_status(ctl, "journal", "live");
    }
    await_prompt(ctl, None);
    ctl.type_line(
        "seq -f 'line %g' 1 5000; \
         printf '\\033[?1049h\\033[2Jon the alternate screen\\033[?1049l'; \
         printf '\\033[?2004h\\033[>5u'; echo ARMED-$((40+2))",
    );
    ctl.expect_on_screen("ARMED-42");
    await_prompt(ctl, Some("ARMED-42"));
    if !seam {
        await_status(ctl, "journal_tail", "0");
        await_status(ctl, "journal_ring_lag", "0");
    }
    await_keeper(root, "claimed=1");
    await_keeper(root, if seam { "journals=0" } else { "journals=1" });
    screen_and_modes(ctl)
}

/// A SIGKILLed by its own pid (never `pkill`), then the keeper's word that the session is
/// an orphan, then B booted in the same world with `seams`: B's recovered session, on a
/// connection made once B's recovery-count fence has lifted ([`admitted`]).
fn crash_and_recover(root: &Path, a: Proc, seams: &[(&str, &str)]) -> (Proc, Ctl) {
    let mut a = a;
    let pid = a.child.id();
    a.child.kill().expect("SIGKILL A");
    let status = a.child.wait().expect("reap A");
    assert!(
        !status.success(),
        "A died by its signal: {status:?} (pid {pid})"
    );
    drop(a);
    await_keeper(root, "orphaned=1");
    let tag = format!("after-{pid}");
    let (b, sock) = boot(root, &tag, seams);
    let ctl = admitted(&sock);
    (b, ctl)
}

/// What every request to a recovered instance answers while an old journal the keeper
/// may still offer has no accepted successor (`keeper_link::RecoveryCountAdmission`):
/// the instance serves nothing and closes the connection until the keeper confirms it
/// read each adopted session's new journal (its SYNC answer behind the COMPANION).
const FENCED: &str = "ERR recovery-count-custody unconfirmed";

/// A connection to the instance at `sock` once its recovery-count fence has lifted
/// ([`FENCED`]): a must-happen wait, each refused connection dropped and a new one asked.
#[track_caller]
fn admitted(sock: &str) -> Ctl {
    let mut last = String::new();
    let ctl = aterm_spec::wait::within(HANG, GAP, || {
        let mut ctl = Ctl::connect(sock);
        last = ctl.line("status");
        (last != FENCED).then_some(ctl)
    });
    ctl.unwrap_or_else(|| panic!("the recovery-count fence never lifted: {last}"))
}

#[test]
fn a_crashed_window_s_session_comes_back_with_its_exact_screen() {
    let world = world("x");
    let root = world.0.clone();
    let _keeper = keeper(&root);
    let (a, sock) = boot(&root, "a", &[]);
    let mut ctl = Ctl::connect(&sock);
    let (screen, modes) = run_a(&root, &mut ctl, false);
    assert!(
        screen.iter().any(|row| row == "ARMED-42"),
        "premise: A's screen holds the marker: {screen:?}"
    );
    assert_eq!(
        modes.get("bracketed_paste").map(String::as_str),
        Some("true")
    );
    assert_ne!(
        modes.get("kitty_keyboard").map(String::as_str),
        Some("none")
    );
    let (marker_row, first_screen_row) = located(&mut ctl, "ARMED-42");
    assert_eq!(
        Some(marker_row - first_screen_row),
        screen
            .iter()
            .position(|row| row == "ARMED-42")
            .map(|row| row as u64),
        "premise: the marker's row is its screen row"
    );
    let a_lines = scrollback_lines(&mut ctl);
    assert!(
        a_lines > 4_000,
        "premise: A scrolled most of its 5,000 lines off: {a_lines}"
    );
    let a_history = history(&mut ctl, first_screen_row, a_lines);
    assert!(
        a_history.values().any(|line| line == "OK line 1"),
        "premise: A's scrollback reaches its first numbered line"
    );
    drop(ctl);
    let a_pid = a.child.id();
    let a_row = census_row(&root, a_pid);
    assert_eq!(
        (a_row.keeper, a_row.restores),
        (Some(KeeperAtLaunch::On), None)
    );

    let (b, mut ctl) = crash_and_recover(&root, a, &[(HOLD_KEYFRAMES_SEAM, "1")]);
    let b_row = census_row(&root, b.child.id());
    assert_eq!(b_row.keeper, Some(KeeperAtLaunch::On));
    assert_eq!(b_row.prev.pid, Some(a_pid), "B's row follows A's");
    assert_eq!(
        b_row
            .restores
            .map(|r| (r.exact, r.screen, r.meta, r.refused)),
        Some((1, 0, 0, 0))
    );
    let status = await_status(&mut ctl, "restore", "exact");
    let f = fields(&status);
    for (key, want) in [
        ("restore_why", "-"),
        ("in_flight_lost", "0"),
        ("tail_overflow", "0"),
        ("marks_lost", "0"),
        ("resizes_unjournaled", "0"),
        ("history_lost", "0"),
    ] {
        assert_eq!(f.get(key), Some(&want), "{key}: {status}");
    }
    let (restored_screen, restored_modes) = screen_and_modes(&mut ctl);
    assert_eq!(restored_screen, screen, "the screen is A's last one");
    assert_eq!(restored_modes, modes, "the modes are A's last ones");
    assert_eq!(
        located(&mut ctl, "ARMED-42"),
        (marker_row, first_screen_row),
        "the same numbering"
    );
    await_scrollback(&mut ctl, a_lines);
    let b_history = history(&mut ctl, first_screen_row, a_lines);
    for (row, line) in &a_history {
        assert_eq!(b_history.get(row), Some(line), "history row {row} is A's");
    }
    if let Some(older) = (first_screen_row - a_lines).checked_sub(1) {
        assert!(
            ctl.line(&format!("line {older}")).starts_with("ERR"),
            "nothing older than A's history"
        );
    }
    assert_eq!(
        fields(&ctl.status()).get("history_lost"),
        Some(&"0"),
        "the import lost nothing"
    );

    await_status(&mut ctl, "journal", "live");
    await_prompt(&mut ctl, Some("ARMED-42"));
    // `exec`: a forked job-control child can print "child setpgid ... Operation not
    // permitted" after B's screen is read, which C then replays.
    ctl.type_line("printf 'B-ONLY-%s' 43; exec sleep 600");
    ctl.expect_on_screen("B-ONLY-43");
    await_status(&mut ctl, "journal_ring_lag", "0");
    let (b_screen, b_modes) = screen_and_modes(&mut ctl);
    let status = ctl.status();
    assert_ne!(
        fields(&status).get("journal_tail"),
        Some(&"0"),
        "premise: B's crash leaves a tail to replay: {status}"
    );
    let (_, b_first) = located(&mut ctl, "B-ONLY-43");
    let b_lines = scrollback_lines(&mut ctl);
    let b_history = history(&mut ctl, b_first, b_lines);
    await_keeper(&root, "journals=1");
    drop(ctl);

    let (_c, mut ctl) = crash_and_recover(&root, b, &[]);
    let status = await_status(&mut ctl, "restore", "exact");
    let f = fields(&status);
    for (key, want) in [
        ("restore_why", "-"),
        ("in_flight_lost", "0"),
        ("tail_overflow", "0"),
        ("marks_lost", "0"),
        ("resizes_unjournaled", "0"),
        ("history_lost", "0"),
    ] {
        assert_eq!(f.get(key), Some(&want), "C {key}: {status}");
    }
    let (c_screen, c_modes) = screen_and_modes(&mut ctl);
    assert!(
        c_screen.iter().any(|row| row.contains("B-ONLY-43")),
        "C's screen is B's, from the region B handed the keeper: {c_screen:?}"
    );
    assert_eq!(c_screen, b_screen, "the replayed tail is B's last screen");
    assert_eq!(c_modes, b_modes, "and B's last modes");
    await_scrollback(&mut ctl, b_lines);
    let c_history = history(&mut ctl, b_first, b_lines);
    assert_eq!(
        c_history, b_history,
        "B's whole scrollback, across a second crash"
    );
}

/// One `turn` typed into the session at its prompt (`echo <marker>`, settled): its id.
fn drive_turn(ctl: &mut Ctl, marker: &str) -> u64 {
    let (header, _) = ctl.counted(&format!("turn idle=200 timeout=30000 -- echo {marker}"));
    let f = fields(&header);
    assert_eq!(f.get("status"), Some(&"settled"), "{header}");
    f.get("id")
        .and_then(|id| id.parse().ok())
        .unwrap_or_else(|| panic!("an id= in {header}"))
}

/// `history`: its header's fields and its rows.
fn history_rows(ctl: &mut Ctl) -> (String, Vec<String>) {
    ctl.counted("history")
}

/// THE TURN LEDGER ACROSS A REAL CRASH (the crash-restore plan's increment 7): a `turn`
/// driven into A's session, A's journal worker carries the session's ledger into the
/// region's control slot (`journal_ledger_lag=0`, the one-minute hang detector), and A is
/// SIGKILLed.
#[test]
fn a_crashed_window_s_turns_are_still_in_history() {
    let world = world("t");
    let root = world.0.clone();
    let _keeper = keeper(&root);
    let (a, sock) = boot(&root, "a", &[]);
    let mut ctl = Ctl::connect(&sock);
    await_status(&mut ctl, "journal", "live");
    await_prompt(&mut ctl, None);
    let id = drive_turn(&mut ctl, "LEDGER-ONE");
    await_prompt(&mut ctl, Some("LEDGER-ONE"));
    await_status(&mut ctl, "journal_ledger_lag", "0");
    await_keeper(&root, "journals=1");
    let (_, a_rows) = history_rows(&mut ctl);
    assert_eq!(
        a_rows.len(),
        1,
        "premise: A's ledger holds the turn: {a_rows:?}"
    );
    drop(ctl);

    let (_b, mut ctl) = crash_and_recover(&root, a, &[]);
    let (header, rows) = history_rows(&mut ctl);
    assert_eq!(
        rows.len(),
        1,
        "B's history holds A's turn: {header} {rows:?}"
    );
    let row = &rows[0];
    let f = fields(row);
    assert_eq!(
        row.split_whitespace().nth(1),
        Some(id.to_string().as_str()),
        "the turn's own id: {row}"
    );
    assert_eq!(f.get("carried"), Some(&"1"), "{row}");
    assert_eq!(f.get("submitted"), Some(&"1"), "{row}");
    let (_, text) = row.split_once(" text=").expect("text= is the tail");
    assert!(text.contains("LEDGER-ONE"), "{row}");
    let floor: u64 = fields(&header)
        .get("unheld_below")
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("the crash's floor: {header}"));
    assert!(floor > id, "the floor is past the carried turn: {header}");
    await_prompt(&mut ctl, Some("LEDGER-ONE"));
    let next = drive_turn(&mut ctl, "LEDGER-TWO");
    assert!(
        next >= floor,
        "B's turn ids go on past the floor: {next} < {floor}"
    );
}

/// NEGATIVE CONTROL: A's journal off by the seam, so the keeper is told A's session has
/// no region; B's recovery takes the META rung, the screen blank, and the turn A recorded
/// is not in B's `history`.
#[test]
fn without_a_journal_the_recovery_comes_back_blank() {
    let world = world("n");
    let root = world.0.clone();
    let (a, sock) = boot(&root, "a", &[(NO_JOURNAL_SEAM, "1")]);
    let a_row = census_row(&root, a.child.id());
    assert_eq!(
        (a_row.keeper, a_row.restores),
        (Some(KeeperAtLaunch::Absent), None)
    );
    let _keeper = keeper(&root);
    let mut ctl = Ctl::connect(&sock);
    let (screen, _) = run_a(&root, &mut ctl, true);
    assert!(
        screen.iter().any(|row| row == "ARMED-42"),
        "premise: A's screen holds the marker: {screen:?}"
    );
    drive_turn(&mut ctl, "LEDGER-NC");
    let (_, a_rows) = history_rows(&mut ctl);
    assert_eq!(
        a_rows.len(),
        1,
        "premise: A's ledger holds the turn: {a_rows:?}"
    );
    drop(ctl);

    let (b, mut ctl) = crash_and_recover(&root, a, &[]);
    let status = await_status(&mut ctl, "restore", "meta");
    assert_eq!(
        fields(&status).get("restore_why"),
        Some(&"no-region"),
        "{status}"
    );
    let b_row = census_row(&root, b.child.id());
    assert_eq!(b_row.keeper, Some(KeeperAtLaunch::On));
    assert_eq!(
        b_row
            .restores
            .map(|r| ((r.exact, r.screen, r.meta, r.refused), r.whys)),
        Some(((0, 0, 1, 0), vec![("no-region".to_string(), 1)]))
    );
    let blank = ctl.text();
    assert!(
        !blank.iter().any(|row| row.contains("ARMED-42")),
        "the META rung brings no screen back: {blank:?}"
    );
    let (header, rows) = history_rows(&mut ctl);
    assert!(
        rows.is_empty(),
        "no region, no ledger: B's history is empty: {header} {rows:?}"
    );
}

/// THE MEMORY SHED ACROSS A REAL CRASH (the crash-restore plan's increment 6): A's session
/// journals its screen and 5,000 lines of scrollback, then memory pressure (`pressure
/// critical`, the injection a development build takes in place of macOS's source) takes
/// its history ring away: the session's region is replaced by a fresh one whose ring is
/// never written (`journal_ring=shedding` until the keeper confirms it read the fresh
/// region, then `shed`, with fewer `journal_bytes=`), and the keeper takes the
/// fresh region (`companions_taken=` moves while `journals=1` holds).
#[test]
fn a_session_shed_under_memory_pressure_comes_back_with_its_exact_screen() {
    let world = world("p");
    let root = world.0.clone();
    let _keeper = keeper(&root);
    let (a, sock) = boot(&root, "a", &[]);
    let mut ctl = Ctl::connect(&sock);
    run_a(&root, &mut ctl, false);
    let turn_id = drive_turn(&mut ctl, "LEDGER-SHED");
    await_prompt(&mut ctl, Some("LEDGER-SHED"));
    await_status(&mut ctl, "journal_ledger_lag", "0");
    await_status(&mut ctl, "journal_tail", "0");
    await_status(&mut ctl, "journal_ring_lag", "0");
    let number = |status: &str, key: &str| -> u64 {
        fields(status)
            .get(key)
            .and_then(|n| n.parse().ok())
            .unwrap_or_else(|| panic!("`{key}=` in {status}"))
    };
    let before = ctl.status();
    assert_eq!(
        fields(&before).get("journal_ring"),
        Some(&"live"),
        "{before}"
    );
    await_keeper(&root, "companions_taken=1");

    let reply = ctl.ok("pressure critical");
    let f = fields(&reply);
    assert_eq!(f.get("level"), Some(&"critical"), "{reply}");
    assert_eq!(f.get("rings_shed"), Some(&"1"), "{reply}");
    let shed = await_status(&mut ctl, "journal_ring", "shed");
    assert!(
        number(&shed, "journal_bytes") < number(&before, "journal_bytes"),
        "the ring's pages left the window: {before} / {shed}"
    );
    await_keeper(&root, "companions_taken=2");
    await_keeper(&root, "journals=1");
    let (screen, modes) = screen_and_modes(&mut ctl);
    let a_lines = scrollback_lines(&mut ctl);
    assert!(a_lines > 4_000, "premise: A's scrollback: {a_lines}");
    drop(ctl);

    let (_b, mut ctl) = crash_and_recover(&root, a, &[]);
    let status = await_status(&mut ctl, "restore", "exact");
    let f = fields(&status);
    for (key, want) in [
        ("restore_why", "-"),
        ("in_flight_lost", "0"),
        ("tail_overflow", "0"),
        ("marks_lost", "0"),
        ("resizes_unjournaled", "0"),
    ] {
        assert_eq!(f.get(key), Some(&want), "{key}: {status}");
    }
    let (restored_screen, restored_modes) = screen_and_modes(&mut ctl);
    assert_eq!(restored_screen, screen, "the screen is A's last one");
    assert_eq!(restored_modes, modes, "the modes are A's last ones");
    let b_lines = scrollback_lines(&mut ctl);
    assert!(
        b_lines > 0 && b_lines <= 512,
        "B's scrollback is the fresh keyframe's bridge: {b_lines}"
    );
    assert_eq!(
        number(&status, "history_lost"),
        a_lines - b_lines,
        "every line of A's above the bridge is counted lost: {status}"
    );
    let (header, rows) = history_rows(&mut ctl);
    assert_eq!(rows.len(), 1, "A's turn: {header} {rows:?}");
    assert_eq!(
        rows[0].split_whitespace().nth(1),
        Some(turn_id.to_string().as_str()),
        "{rows:?}"
    );
    assert_eq!(fields(&rows[0]).get("carried"), Some(&"1"), "{rows:?}");
}

/// CONTROL for every test here: an instance on a stand-in system held at critical, the level
/// an overloaded host reaches ([`launch_isolation::SYSTEM_PRESSURE`] set to `critical` in place
/// of `none`), is at critical with no test driving it, and its session's history ring is shed
/// from the start, so a wait for `journal_ring_lag=0` would hang, as the exact-screen and shed
/// tests do when their instances hear such a system.
#[test]
fn an_instance_hearing_a_system_at_critical_sheds_its_ring() {
    let world = world("h");
    let root = world.0.clone();
    let _keeper = keeper(&root);
    let (_a, sock) = boot(
        &root,
        "a",
        &[(launch_isolation::SYSTEM_PRESSURE, "critical")],
    );
    let mut ctl = Ctl::connect(&sock);
    await_status(&mut ctl, "journal", "live");
    let mut last = String::new();
    let heard = aterm_spec::wait::within(HANG, GAP, || {
        last = ctl.ok("pressure");
        (fields(&last).get("level") == Some(&"critical")).then_some(())
    });
    assert!(
        heard.is_some(),
        "the stand-in system never reached the instance: {last}"
    );
    await_status(&mut ctl, "journal_ring", "shed");
}

/// A keeper has run once its copy of its code holds the mark it writes as it starts from it,
/// or once it wrote to its log, never merely once it was spawned: the first exec of its copy is
/// macOS's to finish. Negative controls: a half-made copy's mark, another keeper's (pids 42
/// and 424 beside 4242) and an empty log are no proof.
#[test]
fn a_keeper_has_run_once_its_copy_says_so_or_it_wrote_a_line() {
    let scratch = aterm_tempfile::Builder::new()
        .prefix("akran-")
        .tempdir()
        .expect("a scratch world");
    let root = scratch.path();
    let copy = |name: &str, ran: bool| {
        let dir = keeper_code(root).join(name);
        std::fs::create_dir_all(&dir).expect("a copy");
        if ran {
            aterm_keeper::code::mark_ran(&dir);
        }
    };
    let keeper_ran = |pid| keeper_reached(root, pid, aterm_keeper::code::RAN);
    std::fs::write(root.join("keeper.log"), "").expect("the log");
    copy("4242-1792000000-7", false);
    copy("4242-1792000000-8.partial", true);
    copy("42-1792000000-7", true);
    assert!(!keeper_ran(4242), "spawned, its copy unmarked");
    assert!(keeper_ran(42), "pid 42's marked copy");
    copy("4242-1792000000-7", true);
    assert!(keeper_ran(4242), "its copy marked");
    assert!(
        !keeper_ran(424),
        "pid 4242's mark is not pid 424's, and its log is empty"
    );
    std::fs::write(root.join("keeper.log"), "aterm keeper serve: not a copy\n").expect("a line");
    assert!(keeper_ran(424), "its line");
}

/// A keeper's start is judged in two waits ([`await_start`]): its own work up to its copy's
/// exec stamp, then macOS's first-exec check of the copy up to the copy's mark. Each bound is
/// zero here, so each wait looks once. Negative controls: no copy, or a copy not yet stamped,
/// is [`Start::NoCopy`] (the first wait's, never the second's), a stamped copy unmarked is
/// [`Start::NotRun`], and an exit or a line in its log ends either wait.
#[test]
fn a_keeper_s_start_waits_for_its_copy_s_stamp_then_its_mark() {
    let scratch = aterm_tempfile::Builder::new()
        .prefix("akstart-")
        .tempdir()
        .expect("a scratch world");
    let root = scratch.path();
    let now = |ended: bool| await_start(root, 4242, Duration::ZERO, Duration::ZERO, &mut || ended);
    std::fs::write(root.join("keeper.log"), "").expect("the log");
    assert_eq!(now(false), Start::NoCopy, "spawned, no copy");
    assert_eq!(now(true), Start::Ended, "exited before it made one");
    let copy = keeper_code(root).join("4242-1792000000-7");
    std::fs::create_dir_all(&copy).expect("a copy");
    assert_eq!(now(false), Start::NoCopy, "copied, not stamped");
    aterm_uds::deadline::stamp_exec(&copy);
    assert_eq!(now(false), Start::NotRun, "stamped, unmarked");
    aterm_keeper::code::mark_ran(&copy);
    assert_eq!(now(false), Start::Ran, "its copy marked");
    std::fs::remove_dir_all(&copy).expect("its copy goes");
    std::fs::write(root.join("keeper.log"), "aterm keeper serve: not a copy\n").expect("a line");
    assert_eq!(now(false), Start::Ran, "its line, no copy");
}

/// A world whose keeper's copy never showed it ran is kept when dropped, the copy left to
/// macOS's assessment of its first exec, however long ago it was stamped (the product's sweep
/// would take a copy that old, [`aterm_uds::deadline::copy_may_go`]). Negative controls: once
/// the copy holds its mark the world goes, beside an entry no keeper named and a half-made
/// `.partial` copy, neither of which ran. (The copy's keeper is gone: this process's pid at
/// another start.)
#[test]
fn a_world_holding_a_keeper_copy_that_never_ran_is_kept() {
    let made = |ran: bool| {
        let root = aterm_tempfile::Builder::new()
            .prefix("akkept-")
            .tempdir()
            .expect("a scratch world")
            .keep();
        let copy = keeper_code(&root).join(format!("{}-1-2", std::process::id()));
        std::fs::create_dir_all(&copy).expect("a copy");
        aterm_uds::deadline::stamp_exec(&copy);
        if ran {
            aterm_keeper::code::mark_ran(&copy);
        }
        (root, copy)
    };
    let kept_when_dropped = |root: PathBuf| {
        drop(World(root.clone()));
        let kept = root.exists();
        // The stand-in copy holds no program, so nothing assesses it: it may go now.
        let _ = std::fs::remove_dir_all(&root);
        kept
    };
    let (unrun, _) = made(false);
    assert!(
        kept_when_dropped(unrun),
        "the world of a copy that never ran is kept"
    );
    let (aged, copy) = made(false);
    std::fs::write(copy.join(aterm_uds::deadline::EXEC_STAMP), "").expect("a blank stamp");
    std::fs::File::open(&copy)
        .and_then(|dir| dir.set_modified(std::time::UNIX_EPOCH + Duration::from_secs(1)))
        .expect("the copy made long ago");
    assert!(
        aterm_uds::deadline::copy_may_go(&copy, std::time::SystemTime::now()),
        "premise: by the product's rule the copy may go"
    );
    assert!(
        kept_when_dropped(aged),
        "the world of a copy that never ran is kept, however old the copy"
    );
    let (ran, copy) = made(true);
    let code = copy.parent().expect("the copies' folder");
    std::fs::write(code.join(".DS_Store"), "").expect("an entry no keeper named");
    std::fs::create_dir_all(code.join(format!("{}-1-3.partial", std::process::id())))
        .expect("a half-made copy");
    drop(World(ran.clone()));
    assert!(!ran.exists(), "the world of a copy that ran goes");
}

/// A world is made new each time ([`world`]): never at a path an earlier one holds, which a
/// test before it with this pid and tag may have kept, and never by removing one. Negative
/// control: each world goes when dropped.
#[test]
fn a_world_is_made_new_never_over_an_earlier_one() {
    let first = world("u");
    let marker = first.0.join("home").join("earlier");
    std::fs::write(&marker, "").expect("a file in the earlier world");
    let second = world("u");
    assert_ne!(first.0, second.0, "two worlds of one tag in one process");
    assert!(marker.exists(), "making a world removed an earlier one");
    let (a, b) = (first.0.clone(), second.0.clone());
    drop(second);
    drop(first);
    assert!(!a.exists() && !b.exists(), "each world goes when dropped");
}
