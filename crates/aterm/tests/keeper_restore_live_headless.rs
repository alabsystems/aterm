// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE EXACT SCREEN AFTER A CRASH, on real processes (the crash-restore plan §7.3's live lane).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use aterm_update::recovery_ledger::{KeeperAtLaunch, Row, ledger_path, read_rows};

use crate::keeper_world::{
    GAP, HANG, Proc, Start, World, admitted, await_keeper, await_start, boot, keeper, keeper_code,
    keeper_reached, ledger_dir, world,
};
use crate::launch_isolation;
use crate::live_ctl::{Ctl, fields};

/// The window's journal off: the negative control's seam (`keeper_link::journal_refusal`).
const NO_JOURNAL_SEAM: &str = "ATERM_DEBUG_NO_CRASH_JOURNAL";

/// The window's keyframes held after the first, so a crash leaves a tail to replay
/// (`crash_restore::HOLD_KEYFRAMES_SEAM`).
const HOLD_KEYFRAMES_SEAM: &str = "ATERM_DEBUG_CRASH_JOURNAL_HOLD_KEYFRAMES";

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

/// The keeper link's worker and the META pump idle an hour (`keeper_link::QUIET_SEAM`), so only
/// a queued op or the keeper's own frame wakes the worker.
const QUIET_SEAM: &str = "ATERM_DEBUG_KEEPER_QUIET_MS";
const AN_HOUR_MS: &str = "3600000";

/// A's sessions in the keeper's custody, each journaling into a region the keeper holds:
/// session 0 and `more` spawned beside it, each read to its prompt and its journal live.
fn sessions_in_custody(root: &Path, ctl: &mut Ctl, more: usize) -> Vec<String> {
    for _ in 0..more {
        let reply = ctl.ok("spawn");
        assert!(
            reply.split_whitespace().any(|t| t.starts_with("s-")),
            "`spawn` names its sid: {reply}"
        );
    }
    let sids = sids(ctl);
    assert_eq!(
        sids.len(),
        more + 1,
        "A runs {} sessions: {sids:?}",
        more + 1
    );
    for sid in &sids {
        await_status_of(ctl, sid, "journal", "live");
    }
    await_keeper(root, &format!("claimed={}", sids.len()));
    await_keeper(root, &format!("journals={}", sids.len()));
    sids
}

/// Every session's sid, from `sessions`.
fn sids(ctl: &mut Ctl) -> Vec<String> {
    let (_, rows) = ctl.counted("sessions");
    rows.iter()
        .filter(|row| row.starts_with(|c: char| c.is_ascii_digit()))
        .filter_map(|row| row.split_whitespace().nth(1).map(str::to_string))
        .collect()
}

/// Wait until session `sid`'s `status` has `key=value`.
#[track_caller]
fn await_status_of(ctl: &mut Ctl, sid: &str, key: &str, value: &str) {
    let mut last = String::new();
    let seen = aterm_spec::wait::within(HANG, GAP, || {
        last = ctl.line(&format!("@{sid} status"));
        (fields(&last).get(key) == Some(&value)).then_some(())
    });
    assert!(seen.is_some(), "{sid}: `{key}={value}` never held: {last}");
}

/// K1 integration coverage: a recovered instance is served after the keeper's SYNC behind
/// the adopted session's new journal. The link's idle wait and META pump period are an hour;
/// this checks successful recovery with periodic work quiet, not every earlier worker schedule.
#[test]
fn a_recovered_window_is_served_once_its_keeper_answers_with_its_link_quiet() {
    let world = world("q");
    let root = world.0.clone();
    let _keeper = keeper(&root);
    let (a, sock) = boot(&root, "a", &[]);
    let mut ctl = Ctl::connect(&sock);
    sessions_in_custody(&root, &mut ctl, 0);
    drop(ctl);
    let (_b, mut ctl) = crash_and_recover(&root, a, &[(QUIET_SEAM, AN_HOUR_MS)]);
    await_status(&mut ctl, "restore", "exact");
}

/// A plain headless instance (no desk seam) that recovers more than one keeper session places
/// every one, so its fence lifts: it has no glass to wait for before its restore pass.
#[test]
fn a_plain_headless_instance_places_every_session_it_recovers() {
    let world = world("two");
    let root = world.0.clone();
    let _keeper = keeper(&root);
    let (a, sock) = boot(&root, "a", &[]);
    let mut ctl = Ctl::connect(&sock);
    let mut a_sids = sessions_in_custody(&root, &mut ctl, 2);
    a_sids.sort_unstable();
    assert!(a_sids.windows(2).all(|pair| pair[0] != pair[1]));
    drop(ctl);
    let mut a = a;
    let pid = a.child.id();
    a.child.kill().expect("SIGKILL A");
    let status = a.child.wait().expect("reap A");
    assert!(!status.success(), "A died by its signal: {status:?}");
    drop(a);
    await_keeper(&root, &format!("orphaned={}", a_sids.len()));
    let (_b, sock) = boot(&root, &format!("after-{pid}"), &[]);
    let mut ctl = admitted(&sock);
    let mut b_sids = sids(&mut ctl);
    b_sids.sort_unstable();
    assert_eq!(b_sids, a_sids, "B placed every session A left: {b_sids:?}");
    for sid in &a_sids {
        await_status_of(&mut ctl, sid, "restore", "exact");
    }
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
