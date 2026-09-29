// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Live-headless check of the supervisor FULLY AUTOMATIC (aterm-agent
//! `supervise`: the approval and turn-end policies, carried out by
//! `Session::run_hosted`) over a real headless instance and its real control
//! socket, through the supervisor's own persistent connection (`RelayCtl`).
//!
//! * FULL POWER, the owner's default: a WRITE box (the measured `touch x`)
//!   gets its one-shot allow (`1`, fenced and guarded); a request for a
//!   decision is ANSWERED with `answer_text`; a worker whose every turn is
//!   short is continued on a GROWING back-off (the timing injected, 1 s
//!   doubled); a turn a person stopped with Esc is held for their grace and
//!   then continued. Each with its negative control (`approve = "safe"`,
//!   `answer_questions = false`, a turn of real work).
//!
//! * A FAKE WORKER (a POSIX `sh` script in raw mode, run as `claude` via
//!   `exec -a`, which appends every byte it reads to a key log and every
//!   submitted composer line to a submit log) draws Claude-shaped screens:
//!   an idle "ready" screen; on a person's line, a busy screen, then the end
//!   of turn under test — the measured `/goal` screen with its dim
//!   suggestion `❯ keep going` (the cursor at column 2), or the hand-built
//!   `API Error: 529 Overloaded` end; on the next line, busy again, then a
//!   turn that ends asking for a decision. It echoes typed text into its
//!   composer row and fills the suggestion on `right`, as Claude Code does.
//! * THE SUGGESTION: the supervisor accepts it with `right` and a fenced
//!   Enter, and the worker receives EXACTLY ONE continuation, `keep going`;
//!   the decision it ends on next is escalated (the keyed attention), never
//!   typed into.
//! * THE 529: the supervisor waits the (shortened) backoff, then types its
//!   retry — Claude Code's own line quoted (`Claude Code reported "API
//!   Error: 529 Overloaded. …". Carry on from where you stopped; …`), never
//!   the rote `keep going` (the outage of 2026-09-27) — through the fenced
//!   write (`send if-gen=` on the judged read, then a fenced Enter once the
//!   composer shows it, wrapped over two rows) — once — and not before the
//!   backoff has run.
//! * A LONG CONTINUATION IN A NARROW COMPOSER (lane B2's review, major 4):
//!   64 columns and a rules file, so `keep going (standing rules: …)` wraps
//!   over three composer rows, the last holding fewer characters than the
//!   old 40-character guard named; it is read back whole and submitted
//!   once.
//!   Negative control: a worker whose composer does not show the text as
//!   written (a pasted-text placeholder) gets no Enter, and the text left
//!   typed is escalated.
//!
//! ISOLATION: scratch HOME/XDG roots, a private control socket, a config
//! with every automatic lane off, `--no-reroute`, `SHELL=/bin/sh`
//! (`support/launch_isolation.rs`); the approval ledger in the scratch root.
//! A headless instance opens no window.

#![cfg(unix)]

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use aterm_agent::supervise::phase::composer_text;
use aterm_agent::supervise::policy::turn_end::TurnEndTiming;
use aterm_agent::supervise::prompt::fixtures::{
    BOX_BASH_TOUCH, END_529, GOAL_ACTIVE_SUGGESTION, composer, screen,
};
use aterm_agent::supervise::{
    Ctl, CtlReply, Endpoint, Interrupter, Phase, RelayCtl, Session, SuperviseOpts,
    SupervisorConfig, is_placeholder, read_journal, worker_phase,
};

#[path = "support/headless_boot.rs"]
mod headless_boot;
#[path = "support/launch_isolation.rs"]
mod launch_isolation;

use headless_boot::{Instance, log_tail};

/// Boot one headless instance, 40 rows, [`COLUMNS`] wide.
fn boot(tag: &str) -> Option<Instance> {
    boot_with(tag, COLUMNS)
}

/// Boot one headless instance, 40 rows, `columns` wide ([`headless_boot::boot`]:
/// `None` is an environment refusal; a product that cannot start fails the test).
fn boot_with(tag: &str, columns: &str) -> Option<Instance> {
    headless_boot::boot(
        &format!("atte{tag}"),
        &["--lines", "40", "--columns", columns],
    )
}

const CLIENT_EXIT_DEADLINE: Duration = Duration::from_secs(60);
/// Wide enough that no drawn row wraps (the 529 notice is 152 columns).
const COLUMNS: &str = "170";

/// One bounded `aterm ctl --sock <sock> <args…>` call.
fn ctl(inst: &Instance, args: &[&str]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_aterm"));
    cmd.arg("ctl")
        .arg("--sock")
        .arg(&inst.sock)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    launch_isolation::apply(&mut cmd, &inst.tmp);
    let mut child = cmd.spawn().expect("spawn aterm ctl");
    fn drain(pipe: Option<impl Read + Send + 'static>) -> std::thread::JoinHandle<Vec<u8>> {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_end(&mut buf);
            }
            buf
        })
    }
    let stdout = drain(child.stdout.take());
    let stderr = drain(child.stderr.take());
    let deadline = Instant::now() + CLIENT_EXIT_DEADLINE;
    loop {
        match child.try_wait().expect("poll aterm ctl") {
            Some(status) => {
                return Output {
                    status,
                    stdout: stdout.join().expect("stdout drain"),
                    stderr: stderr.join().expect("stderr drain"),
                };
            }
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("aterm ctl {args:?} did not exit in time");
            }
            None => std::thread::sleep(Duration::from_millis(10)),
        }
    }
}

fn ctl_ok(inst: &Instance, args: &[&str]) -> String {
    let out = ctl(inst, args);
    assert!(
        out.status.success(),
        "aterm ctl {args:?} failed: stdout={:?} stderr={:?}\ninstance log tail:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
        log_tail(&inst.log),
    );
    String::from_utf8(out.stdout).expect("utf-8")
}

/// The boot session's sid from `sessions`.
fn boot_session(inst: &Instance) -> String {
    let body = ctl_ok(inst, &["sessions"]);
    let row = body
        .lines()
        .find(|l| l.starts_with(|c: char| c.is_ascii_digit()))
        .unwrap_or_else(|| panic!("a session row: {body}"));
    row.split_whitespace().nth(1).unwrap().to_string()
}

/// Type one line into the session and submit it, as a person does.
fn type_line(inst: &Instance, sid: &str, line: &str) {
    ctl_ok(inst, &[&format!("@{sid}"), "send", line]);
    ctl_ok(inst, &[&format!("@{sid}"), "key", "enter"]);
}

/// `await match <re>` on the session: the server's latched watcher. `<re>`
/// is one wire token (`.` for a space).
fn await_match(inst: &Instance, sid: &str, re: &str) {
    if let Err(reply) = awaited(inst, sid, re) {
        panic!("`{re}` never reached the screen: {reply}");
    }
}

/// [`await_match`] handing the reply back (`aterm ctl` exits non-zero on
/// `OK timeout`), so a test holding a [`Supervisor`] can stop it first and
/// say what its loop printed — the one record of which side stalled.
fn awaited(inst: &Instance, sid: &str, re: &str) -> Result<(), String> {
    let out = ctl(
        inst,
        &[&format!("@{sid}"), "await", "match", re, "timeout=20000"],
    );
    let reply = String::from_utf8_lossy(&out.stdout).into_owned();
    if out.status.success() && reply.starts_with("OK") && !reply.starts_with("OK timeout") {
        return Ok(());
    }
    Err(format!(
        "{reply:?} stderr={:?}",
        String::from_utf8_lossy(&out.stderr)
    ))
}

/// [`awaited`] for `timeout_ms`: the regression's control.
fn awaited_for(inst: &Instance, sid: &str, re: &str, timeout_ms: u64) -> Result<(), String> {
    let timeout = format!("timeout={timeout_ms}");
    let out = ctl(inst, &[&format!("@{sid}"), "await", "match", re, &timeout]);
    let reply = String::from_utf8_lossy(&out.stdout).into_owned();
    if out.status.success() && reply.starts_with("OK") && !reply.starts_with("OK timeout") {
        return Ok(());
    }
    Err(reply)
}

/// The fake worker's events of `kind` (`draw-<screen>`, `submit`, `press`)
/// in the order it did them, each stamped as it happened (the mtime of the
/// empty file it created then), once there are `n` of them, or as many as
/// came within `within`.
///
/// This, not [`await_match`], is how a test waits for a screen the loop
/// under test REPLACES — the end of turn it continues, the box it answers,
/// the busy screen of a two-second turn. `await match` sees the screen as it
/// is when the watcher is armed and every change after, never one that came
/// and went before: armed after the loop had already continued (a test
/// process stalled for a few seconds between its `key enter` and its
/// `await`, 2026-09-27 under a gate's load), it waited 20 s for a screen
/// gone for good. The worker's own record is there whenever it is read.
/// The stamps are the worker's too, so an interval measured from them (a
/// back-off, a grace) is not shortened by a test that looked late.
fn stamps_when(events: &Path, kind: &str, n: usize, within: Duration) -> Vec<SystemTime> {
    let deadline = Instant::now() + within;
    loop {
        let mut seen: Vec<(u64, SystemTime)> = std::fs::read_dir(events)
            .map(|d| d.flatten().collect::<Vec<_>>())
            .unwrap_or_default()
            .into_iter()
            .filter_map(|e| {
                let name = e.file_name().into_string().ok()?;
                let (seq, k) = name.split_once('.')?;
                if k != kind {
                    return None;
                }
                Some((seq.parse().ok()?, e.metadata().ok()?.modified().ok()?))
            })
            .collect();
        seen.sort_by_key(|&(seq, _)| seq);
        if seen.len() >= n || Instant::now() >= deadline {
            return seen.into_iter().map(|(_, at)| at).collect();
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// When the worker first drew `screen` (its event record, [`stamps_when`]),
/// waiting up to 20 s; panics naming what it did draw.
fn drawn(events: &Path, screen: &str) -> SystemTime {
    if let Some(&at) = stamps_when(
        events,
        &format!("draw-{screen}"),
        1,
        Duration::from_secs(20),
    )
    .first()
    {
        return at;
    }
    panic!(
        "the worker never drew `{screen}`: it did {:?}",
        record(events)
    );
}

/// The worker's whole event record, in order, each event with its stamp in
/// ms since the epoch — the clock of the loop's journal (`"t"`), so the two
/// read side by side.
fn record(events: &Path) -> Vec<String> {
    let mut seen: Vec<(u64, String)> = std::fs::read_dir(events)
        .map(|d| d.flatten().collect::<Vec<_>>())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let seq = name.split('.').next()?.parse().ok()?;
            let ms = e
                .metadata()
                .ok()?
                .modified()
                .ok()?
                .duration_since(SystemTime::UNIX_EPOCH)
                .ok()?
                .as_millis();
            Some((seq, format!("{name}@{ms}")))
        })
        .collect();
    seen.sort();
    seen.into_iter().map(|(_, e)| e).collect()
}

/// `b - a`, zero when `b` is not later.
fn between(a: SystemTime, b: SystemTime) -> Duration {
    b.duration_since(a).unwrap_or_default()
}

/// Every event in the worker's record ([`stamps_when`]) — its order, its
/// kind, its stamp — in order.
fn worker_events(events: &Path) -> Vec<(u64, String, SystemTime)> {
    let mut seen: Vec<(u64, String, SystemTime)> = std::fs::read_dir(events)
        .map(|d| d.flatten().collect::<Vec<_>>())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let (seq, kind) = name.split_once('.')?;
            let at = e.metadata().ok()?.modified().ok()?;
            Some((seq.parse().ok()?, kind.to_string(), at))
        })
        .collect();
    seen.sort_by_key(|(seq, _, _)| *seq);
    seen
}

/// When the first bytes the worker read after its `n`-th (1-based) drawing
/// of `screen` reached it (its `key` event, [`FAKE_WORKER`]): the first key
/// of what the loop typed at that point — the retry, the continuation —
/// waiting up to `within` for it. Nothing else writes to the worker while a
/// test's loop decides a point.
fn first_key_after(events: &Path, screen: &str, n: usize, within: Duration) -> Option<SystemTime> {
    let draw = format!("draw-{screen}");
    let deadline = Instant::now() + within;
    loop {
        let all = worker_events(events);
        let from = all
            .iter()
            .filter(|(_, kind, _)| *kind == draw)
            .nth(n.checked_sub(1)?)
            .map(|(seq, _, _)| *seq);
        let key = from.and_then(|from| {
            all.iter()
                .find(|(seq, kind, _)| *seq > from && kind == "key")
                .map(|&(_, _, at)| at)
        });
        if key.is_some() || Instant::now() >= deadline {
            return key;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// When the loop under test REVIEWED the first point it met at or after
/// `drawn` (a worker stamp), by the loop's own clock: the journal's `t` of
/// its first `EVENT` line from then on, with the line. The loop folds a new
/// point into its policy — the instant every back-off, grace and retry wait
/// of the turn-end policy counts from (`TurnEndState::observe`) — and
/// journals the point's `EVENT` line right after, before anything is typed
/// at it; waiting, it types nothing at all. So the interval from this to
/// [`first_key_after`] is the loop's own wait at the point, and nothing
/// else: not the loop's latency in seeing the screen, not the typing, the
/// read-back settle or the Enter after it. (The `t` is read a few
/// microseconds of the loop's work after the instant the wait counts from,
/// so the interval is the wait less that — inside every tolerance here.) An interval measured from the
/// worker's drawing to its taking the line held all of those (2.5 s of them
/// on 2026-09-28), and a lower bound on it passed with the wait removed.
fn reviewed_at(inst: &Instance, drawn: SystemTime) -> Option<(SystemTime, String)> {
    let floor = drawn
        .duration_since(SystemTime::UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_millis()).ok())?;
    let (records, _) = read_journal(&inst.tmp.join("state/journal.jsonl")).ok()?;
    records
        .into_iter()
        .find(|r| r.kind == "event" && r.t >= floor)
        .map(|r| {
            let ms = u64::try_from(r.t).unwrap_or_default();
            (SystemTime::UNIX_EPOCH + Duration::from_millis(ms), r.line)
        })
}

/// [`reviewed_at`] to [`first_key_after`]: how long the loop waited at the
/// point the worker drew `n`-th as `screen` (drawn at `drawn`) before it
/// typed, and the point's `EVENT` line; zero when either is missing.
fn waited_at(
    inst: &Instance,
    events: &Path,
    screen: &str,
    n: usize,
    drawn: SystemTime,
) -> (Duration, String) {
    let key = first_key_after(events, screen, n, Duration::from_secs(1));
    match (reviewed_at(inst, drawn), key) {
        (Some((at, line)), Some(key)) => (between(at, key), line),
        (reviewed, key) => (
            Duration::ZERO,
            format!("no interval: reviewed {reviewed:?}, first key {key:?}"),
        ),
    }
}

/// The lines of `path` once it holds `n` of them, or panic after `within`
/// (the worker's own log: nothing on the socket announces it).
fn lines_when(path: &Path, n: usize, within: Duration) -> Vec<String> {
    let deadline = Instant::now() + within;
    loop {
        let lines: Vec<String> = std::fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect();
        if lines.len() >= n || Instant::now() >= deadline {
            return lines;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Rows as the fake worker draws them: each row at its own line, `\r\n`
/// between, then the cursor placed at (`row`, `col`), both 0-based.
fn render(rows: &[String], cursor: (usize, usize)) -> String {
    let mut s = rows.join("\r\n");
    s.push_str(&format!("\x1b[{};{}H", cursor.0 + 1, cursor.1 + 1));
    s
}

/// The index of the composer's caret row.
fn caret(rows: &[String]) -> usize {
    rows.iter()
        .rposition(|r| r.starts_with('❯'))
        .expect("a caret row")
}

/// A finished turn: `said`, a done row after 3 minutes of work, the composer.
fn ended(said: &str) -> Vec<String> {
    let mut r: Vec<String> = [
        format!("⏺ {said}"),
        String::new(),
        "✻ Worked for 3m 2s · done 4:24 PM".to_string(),
        String::new(),
    ]
    .to_vec();
    r.extend(composer("  ⏵⏵ bypass permissions on (shift+tab to cycle)"));
    r
}

fn busy() -> Vec<String> {
    let mut r: Vec<String> = [
        "⏺ On it.".to_string(),
        String::new(),
        "✶ Deliberating… (4s · ↓ 120 tokens · esc to interrupt)".to_string(),
        String::new(),
    ]
    .to_vec();
    r.extend(composer("  ⏵⏵ bypass permissions on · esc to interrupt"));
    r
}

/// The measured `/goal` screen (2026-09-21) at the END of its turn: the
/// running workflow's status row, its monitor and its agent rows gone, a
/// done row in their place, `◎ /goal active` and the dim `❯ keep going`
/// kept as measured, a `⏺` head over the message's last rows.
fn goal_end() -> Vec<String> {
    let measured = screen(GOAL_ACTIVE_SUGGESTION);
    let mut r = vec!["⏺ Stage 2 of the harness is in.".to_string()];
    for row in &measured {
        if row.starts_with("✻ Waiting for") {
            r.push("✻ Worked for 3m 2s · done 4:24 PM".to_string());
        } else if row.trim_start().starts_with('◯') || row.trim_start().starts_with('⧉') {
            continue;
        } else if row.contains("1 monitor") {
            r.push("  ⏵⏵ bypass permissions on · ← for agents".to_string());
        } else {
            r.push(row.clone());
        }
    }
    r
}

/// The decision the second turn ends on: answered under full power,
/// escalated (never typed into) where the owner switched answers off.
const STOP: &str = "I need your decision on the schema before I go on.";

/// A turn a person stopped with Esc, as Claude Code 2.1.280 draws it.
fn interrupted() -> Vec<String> {
    let mut r: Vec<String> = [
        "⏺ Running the schema migration against the staging database now.",
        "  ⎿  Interrupted · What should Claude do instead?",
        "",
    ]
    .map(str::to_string)
    .to_vec();
    r.extend(composer("  ⏵⏵ bypass permissions on (shift+tab to cycle)"));
    r
}

/// A plain end of turn after real work: continued.
const DONE: &str = "Fixed the parser; the suite is green.";

/// The fake worker (module header): `$1` the screens' directory (`<name>.scr`
/// drawn whole, `<name>.row` its caret row, 1-based, `<name>.below` the rows
/// under it), `$2` the key log, `$3` the submit log, `$4` the screen the
/// first person's line ends on, `$5` the columns, `$6` `mangle` to show a
/// composer text over 40 characters as a pasted-text placeholder, `$7`
/// `loop` to end EVERY turn on `$4` (else the turns after the first end on
/// the decision), `$8` the event directory ([`drawn`]), `$9` the directory
/// the loop's connection marks its busy reads in ([`Tap`]).
///
/// A line taken is a 2 s busy spell of work THE LOOP READ: while a loop
/// watches, the 2 s run from its read of the busy screen, not from the
/// drawing (`work`). The loop judges a turn by the busy work it read
/// (`TurnEndState::observe`), and a spell of a fixed 2 s from the drawing
/// came and went unread under a read stalled past it — the goal end then
/// read as no work (a 2 min back-off), the decision after a continuation as
/// the continuation not yet taken (30 s) — and a test waiting 20 s for the
/// act failed ([`a_loop_stalled_in_a_turn_still_reads_the_work_it_judges`]).
///
/// On the `box` screen a digit answers the box — logged `PRESS:<digit>` —
/// and the turn ends on `done` after a fixed 1 s spell: no test judges the
/// turn after a box.
///
/// The composer is redrawn once per READ of the terminal, as Claude Code
/// renders a burst of input in one frame — in two frames, half the new text
/// and then all of it: `❯ <text>` from the caret row (the terminal wraps it), the rows
/// under it moved down past it, the cursor after the text. Never once per
/// BYTE (2026-09-24): a redraw cost three spawns (`dd`, two `cat`s), so the
/// 150-character continuation cost ~450 — 1.8-3.7 s at a concurrent gate's
/// load of 20-43 — and outran the supervisor's read-back settle
/// (`SETTLE_CAP`, 1.5 s): it read part of the text, pressed no Enter, and the
/// decision screen never came. And no spawn INSIDE a frame (third audit,
/// 2026-09-24): the caret row and the rows under it are read once per screen,
/// so both frames are written by builtins alone — a spawn stalled past the
/// 500 ms settle between clearing the composer and drawing its text latched
/// the supervisor on a half-drawn composer. The two frames are therefore back
/// to back, and this test does not claim to catch a supervisor that reads
/// without settling: the settle itself is pinned by the scripted
/// `await idle 500 timeout 1500` exchanges in `aterm-agent`'s supervise tests.
const FAKE_WORKER: &str = r#"#!/bin/sh
dir="$1"; keys="$2"; subs="$3"; after="$4"; cols="$5"; mangle="$6"; mode="$7"
events="$8"; seen="$9"
stty raw -echo
: > "$keys"; : > "$subs"
exec 3<&0
cr=$(printf '\r'); esc=$(printf '\033')
cur=ready; buf=""; ev=0
# One empty file per event, `<n>.<kind>`, created by a builtin redirect as the
# event happens: its name orders it and its mtime stamps it (the test reads
# them back, `stamps_when`). A
# screen is marked BEFORE it is drawn, a submit AFTER its Enter was read.
mark() { ev=$((ev + 1)); : > "$events/$ev.$1"; }
# The caret row and the rows under it are read once per SCREEN, so a composer
# frame is written by builtins alone: no spawn can stall between clearing the
# composer and drawing its text, or between the half frame and the whole one,
# for longer than the supervisor's 500 ms settle.
draw() {
  mark "draw-$1"; cur="$1"; printf '\033[H\033[2J'; cat "$dir/$1.scr"
  # The box is answered by a key, never composed into: it has no composer.
  [ -e "$dir/$1.row" ] || return 0
  IFS= read -r row < "$dir/$1.row"
  below=$(cat "$dir/$1.below"; printf .); below="${below%.}"
}
# A busy spell of $1 s. While a loop watches (its connection made "$seen"
# before this spell began, `Tap`), the seconds run from when the loop has READ
# the busy screen — the request it makes only after a read that counted the
# turn's work marks "$seen/<n>" for the spell drawn as event n — so a loop
# whose read stalls past the spell still reads it, and reads 2 s of work.
# Unwatched, they run from the drawing.
work() {
  watched=0; [ -d "$seen" ] && watched=1
  draw busy
  if [ "$watched" = 1 ]; then
    while [ ! -e "$seen/$ev" ]; do sleep 0.05; done
  fi
  sleep "$1"
}
compose() {
  shown="$buf"
  if [ "$mangle" = mangle ] && [ ${#buf} -gt 40 ]; then shown="[Pasted text #1]"; fi
  n=$(( (2 + ${#shown} + cols - 1) / cols )); [ "$n" -lt 1 ] && n=1
  printf '\033[%s;1H\033[J\033[%s;1H%s\033[%s;1H❯ %s' \
    "$row" "$((row + n))" "$below" "$row" "$shown"
}
# Every read that brought bytes is an event too, `key`, stamped as soon as it
# returns: the first after a screen is when what the loop typed there reached
# the worker (`first_key_after`).
fill() {
  got=$(dd bs=512 count=1 <&3 2>/dev/null)
  [ -n "$got" ] && mark key
  printf '%s' "$got" >> "$keys"
  chunk="$chunk$got"
}
frames() {
  full="$buf"; half=$(( ${#full} / 2 ))
  if [ "$half" -gt 0 ]; then
    buf=$(printf '%s' "$full" | cut -c1-"$half"); compose
  fi
  buf="$full"; compose
}
draw ready
while :; do
  chunk=""; fill
  [ -z "$chunk" ] && continue
  typed=0
  while [ -n "$chunk" ]; do
    rest="${chunk#?}"; c="${chunk%"$rest"}"; chunk="$rest"
    if [ "$cur" = box ]; then
      case "$c" in [0-9]) printf 'PRESS:%s\n' "$c" >> "$subs"; mark press; draw busy; sleep 1; draw done ;; esac
      continue
    fi
    if [ "$c" = "$cr" ]; then
      printf 'SUBMIT:%s\n' "$buf" >> "$subs"; mark submit
      buf=""; typed=0
      n=$(wc -l < "$subs" | tr -d ' ')
      work 2
      if [ "$n" = 1 ] || [ "$mode" = loop ]; then draw "$after"; else draw stop; fi
    elif [ "$c" = "$esc" ]; then
      while [ ${#chunk} -lt 2 ]; do fill; done
      rest="${chunk#??}"; csi="${chunk%"$rest"}"; chunk="$rest"
      if [ "$csi" = "[C" ] && [ -z "$buf" ] && [ "$cur" = goal ]; then
        buf="keep going"; typed=1
      fi
    else
      buf="$buf$c"; typed=1
    fi
  done
  if [ "$typed" = 1 ]; then frames; fi
done
"#;

/// The fake worker's three logs: the keys it read, the lines it took, and
/// its event directory ([`stamps_when`]).
type WorkerLogs = (PathBuf, PathBuf, PathBuf);

/// Write the screens and the script, start the worker as `claude` in the
/// boot session, and wait for its ready screen.
fn start_worker(inst: &Instance, sid: &str, after: &str) -> WorkerLogs {
    start_worker_with(inst, sid, after, COLUMNS, false)
}

/// [`start_worker`], every turn ending on `after`.
fn start_looping_worker(inst: &Instance, sid: &str, after: &str) -> WorkerLogs {
    start_worker_in(inst, sid, after, COLUMNS, false, true)
}

/// [`start_worker`] for an instance `columns` wide (every rule drawn that
/// wide), the composer mangled past 40 characters when `mangle`.
fn start_worker_with(
    inst: &Instance,
    sid: &str,
    after: &str,
    columns: &str,
    mangle: bool,
) -> WorkerLogs {
    start_worker_in(inst, sid, after, columns, mangle, false)
}

fn start_worker_in(
    inst: &Instance,
    sid: &str,
    after: &str,
    columns: &str,
    mangle: bool,
    looping: bool,
) -> WorkerLogs {
    let dir = inst.tmp.join("screens");
    std::fs::create_dir_all(&dir).expect("screens dir");
    let width: usize = columns.parse().expect("columns");
    let e529 = screen(END_529);
    for (name, rows) in [
        ("ready", ended("Ready for the next stage.")),
        ("busy", busy()),
        ("goal", goal_end()),
        ("e529", e529),
        ("done", ended(DONE)),
        ("stop", ended(STOP)),
        ("interrupted", interrupted()),
        ("box", screen(BOX_BASH_TOUCH)),
    ] {
        let rows: Vec<String> = rows
            .into_iter()
            .map(|r| {
                if !r.is_empty() && r.chars().all(|c| c == '─') {
                    "─".repeat(width.min(r.chars().count()))
                } else {
                    r
                }
            })
            .collect();
        if name == "box" {
            // Answered by a key, never composed into.
            std::fs::write(dir.join("box.scr"), render(&rows, (0, 0))).expect("scr");
            continue;
        }
        let c = caret(&rows);
        let mut drawn = rows.clone();
        if name == "goal" {
            // The suggestion DIM, as Claude Code draws it: the loop reads the
            // `cell` at column 2 before it takes one row of text there for the
            // placeholder, and a plain one is a person's draft, caret homed.
            drawn[c] = drawn[c].replacen("❯ ", "❯ \x1b[2m", 1) + "\x1b[22m";
        }
        std::fs::write(dir.join(format!("{name}.scr")), render(&drawn, (c, 2))).expect("scr");
        std::fs::write(dir.join(format!("{name}.row")), (c + 1).to_string()).expect("row");
        std::fs::write(
            dir.join(format!("{name}.below")),
            rows[c + 1..].join("\r\n"),
        )
        .expect("below");
    }
    let script = inst.tmp.join("fake.sh");
    std::fs::write(&script, FAKE_WORKER).expect("write the fake worker");
    let keys = inst.tmp.join("keys.log");
    let subs = inst.tmp.join("submits.log");
    let events = inst.tmp.join("events");
    std::fs::create_dir_all(&events).expect("events dir");
    type_line(
        inst,
        sid,
        &format!(
            "/bin/bash -c 'exec -a claude /bin/sh {} {} {} {} {after} {columns} {} {} {} {}'",
            script.display(),
            dir.display(),
            keys.display(),
            subs.display(),
            if mangle { "mangle" } else { "-" },
            if looping { "loop" } else { "once" },
            events.display(),
            inst.tmp.join("seen").display(),
        ),
    );
    // The ready screen stays until a line is typed: watching it is sound.
    await_match(inst, sid, "Ready.for.the.next.stage");
    (keys, subs, events)
}

/// The hosted loop on its own thread, over its own persistent connection,
/// under a policy with `timing`; stopped by [`Supervisor::stop`], which
/// returns what the loop printed.
struct Supervisor {
    stop: Arc<AtomicBool>,
    cut: Option<Interrupter>,
    handle: std::thread::JoinHandle<String>,
    /// When each read [`Forcing::held_after`] held was let go.
    released: Arc<Mutex<Vec<SystemTime>>>,
}

/// What a regression makes the loop do that a loaded machine can: a thread
/// scheduled late, a read stalled in the middle of a turn.
#[derive(Clone, Default)]
struct Forcing {
    /// The loop's thread sleeps this long before its first request.
    late: Duration,
    /// For each `n` here, the loop's first screen read after the worker took
    /// its `n`-th line is held until [`Forcing::hold`] after the worker drew
    /// that turn's busy screen.
    held_after: Vec<usize>,
    hold: Duration,
}

/// The loop's connection: every request carried to the instance as it is,
/// the reads [`Forcing::held_after`] names held, and the worker told when
/// the loop has read its busy screen ([`FAKE_WORKER`]'s `work`).
///
/// "Read" is the loop's own: a screen read showing the busy spell, then
/// `await gone` — what the loop asks for after a busy read that counted the
/// turn's work (`await_turn_from`: the work counted from it, then the wait
/// for the busy row to leave), and after no other read these tests make the
/// loop do. A read of the busy screen elsewhere (the wait past a point,
/// `moved_past`) is followed by `await seq`, and the `await gone` a look
/// may begin with (`gone_first`) follows no read of the spell: neither tells
/// the worker. It is marked `seen/<n>`, `n` the worker's event for the
/// newest busy spell: the worker draws the next one only after a line the
/// loop types, so that is the spell the loop read.
struct Tap {
    ctl: RelayCtl,
    events: PathBuf,
    seen: PathBuf,
    forcing: Forcing,
    /// The worker's lines already seen when a read went out.
    took: usize,
    /// The last screen read showed the busy spell.
    read_busy: bool,
    released: Arc<Mutex<Vec<SystemTime>>>,
}

impl Tap {
    /// Tell the worker the loop has read its newest busy spell.
    fn saw_busy(&self) {
        let spell = worker_events(&self.events)
            .into_iter()
            .filter(|(_, kind, _)| kind == "draw-busy")
            .map(|(seq, _, _)| seq)
            .next_back();
        if let Some(seq) = spell {
            std::fs::write(self.seen.join(seq.to_string()), "").expect("mark the busy read");
        }
    }
}

impl Ctl for Tap {
    fn call(&mut self, args: &[&str]) -> Result<CtlReply, String> {
        let mut words = args.iter().filter(|a| !a.starts_with('@')).copied();
        let verb = words.next();
        if verb == Some("await") && words.next() == Some("gone") && self.read_busy {
            self.saw_busy();
        }
        if verb == Some("text") {
            let took = stamps_when(&self.events, "submit", 0, Duration::ZERO).len();
            if took > self.took {
                self.took = took;
                if self.forcing.held_after.contains(&took) {
                    let busy =
                        stamps_when(&self.events, "draw-busy", took, Duration::from_secs(20));
                    if let Some(&drawn) = busy.get(took - 1) {
                        let until = drawn + self.forcing.hold;
                        if let Ok(left) = until.duration_since(SystemTime::now()) {
                            std::thread::sleep(left);
                        }
                    }
                    self.released
                        .lock()
                        .expect("the releases")
                        .push(SystemTime::now());
                }
            }
        }
        let reply = self.ctl.call(args);
        if verb == Some("text") {
            self.read_busy = matches!(&reply, Ok(r) if r.ok() && r.stdout.contains("Deliberating"));
        }
        reply
    }

    fn interrupter(&self) -> Option<Interrupter> {
        self.ctl.interrupter()
    }
}

impl Supervisor {
    /// The owner's policy with the answers off: these scripts end on a
    /// decision, which is then escalated (the answer is
    /// [`a_decision_is_answered_with_the_answer_text`]'s).
    fn start(inst: &Instance, sid: &str, timing: TurnEndTiming) -> Self {
        Self::start_with(inst, sid, timing, no_answers())
    }

    /// [`Self::start`] under `policy`.
    fn start_with(
        inst: &Instance,
        sid: &str,
        timing: TurnEndTiming,
        policy: SupervisorConfig,
    ) -> Self {
        Self::start_forced(inst, sid, timing, policy, Forcing::default())
    }

    /// [`Self::start_with`] under a regression's `forcing` (as under a
    /// gate's load). Returns once the loop has LOOKED — its journal holds
    /// the first point it read ([`Self::looked`]).
    fn start_forced(
        inst: &Instance,
        sid: &str,
        timing: TurnEndTiming,
        policy: SupervisorConfig,
        forcing: Forcing,
    ) -> Self {
        let before = looks(&journal(inst));
        let stop = Arc::new(AtomicBool::new(false));
        let mut relay = RelayCtl::new(Endpoint::Socket(inst.sock.clone()), None);
        relay.connect().expect("the supervisor's connection");
        let late = forcing.late;
        let released = Arc::new(Mutex::new(Vec::new()));
        // From here on the worker's busy spells wait for the loop's read.
        let seen = inst.tmp.join("seen");
        std::fs::create_dir_all(&seen).expect("the busy reads' dir");
        let mut ctl = Tap {
            ctl: relay,
            events: inst.tmp.join("events"),
            seen,
            forcing,
            took: 0,
            read_busy: false,
            released: Arc::clone(&released),
        };
        let cut = ctl.interrupter();
        let ledger = inst.tmp.join("state/drive.jsonl");
        let journal = inst.tmp.join("state/journal.jsonl");
        let sid = format!("@{sid}");
        let flag = Arc::clone(&stop);
        let handle = std::thread::spawn(move || {
            std::thread::sleep(late);
            let mut s = Session::new(&mut ctl, Some(sid));
            s.set_approval_ledger(Some(ledger));
            s.set_supervisor_name(Some("turn-end-live".to_string()));
            s.set_turn_end_timing(timing);
            let opts = SuperviseOpts {
                journal: Some(journal),
                ..SuperviseOpts::hosted_with(&policy)
            };
            let mut out: Vec<u8> = Vec::new();
            let _ = s.run_hosted(&opts, flag, &mut out);
            String::from_utf8(out).unwrap_or_default()
        });
        Self {
            stop,
            cut,
            handle,
            released,
        }
        .looked(inst, before)
    }

    /// When each read the forcing held was let go, in order.
    fn released(&self) -> Vec<SystemTime> {
        self.released.lock().expect("the releases").clone()
    }

    /// Wait until the loop has read the screen and journaled the point it
    /// shows (more `EVENT`s than the `before` there were), or panic with
    /// what it said.
    ///
    /// The tests type their `go` only after this, because the loop judges a
    /// turn by the busy work it SAW: a point it reads first — its work
    /// unknown — is a short turn, backed off by `short_backoff` (2 min by
    /// default) before anything is typed (`TurnEndState::observe`, "the
    /// first point seen"). `start` used to return once the thread was
    /// spawned, and the test typed `go` at once; a loop thread that took
    /// longer than the worker's 2 s turn to make its first read (the
    /// suggestion test under a gate's load of 35-41, 2026-09-28) read the
    /// goal end as its first point and waited 2 min, and the decision never
    /// came (`OK timeout`). Watching from the ready screen, the loop reads
    /// `go`'s busy turn, as a supervisor that was there when it began does.
    fn looked(self, inst: &Instance, before: usize) -> Self {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if looks(&journal(inst)) > before {
                return self;
            }
            if self.handle.is_finished() || Instant::now() >= deadline {
                let out = self.stop();
                panic!(
                    "the loop never journaled a point it read\n{}",
                    evidence(inst, &out)
                );
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// [`await_match`] for a screen that STAYS once drawn, with the loop
    /// running: on `OK timeout` the loop is stopped, and the panic says
    /// everything that names the side that stalled ([`evidence`]).
    fn saw(self, inst: &Instance, sid: &str, re: &str) -> Self {
        match awaited(inst, sid, re) {
            Ok(()) => self,
            Err(reply) => {
                let out = self.stop();
                panic!(
                    "`{re}` never reached the screen: {reply}\n{}",
                    evidence(inst, &out)
                );
            }
        }
    }

    fn stop(self) -> String {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(cut) = &self.cut {
            cut();
        }
        self.handle.join().expect("the supervisor thread")
    }
}

/// The owner's policy, every power on but the answers.
fn no_answers() -> SupervisorConfig {
    SupervisorConfig {
        answer_questions: false,
        ..SupervisorConfig::default()
    }
}

/// The session's `meta` once it satisfies `ok`, or after 20 s as it is.
fn meta_when(inst: &Instance, sid: &str, ok: impl Fn(&str) -> bool) -> String {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let m = ctl_ok(inst, &[&format!("@{sid}"), "meta"]);
        if ok(&m) || Instant::now() >= deadline {
            return m;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The loop's journal (the `WAITING` lines are there, not on its stdout).
fn journal(inst: &Instance) -> String {
    std::fs::read_to_string(inst.tmp.join("state/journal.jsonl")).unwrap_or_default()
}

/// The points the loop read, as its journal holds them (`EVENT …`).
fn looks(journal: &str) -> usize {
    journal
        .lines()
        .filter(|l| l.contains(r#""kind":"event""#))
        .count()
}

/// What a failed wait says: the loop's output and journal (`"t"` its
/// stamps), the lines and keys the worker took, and the worker's event
/// record on the journal's clock ([`record`]) — enough to name which side
/// stalled, and where.
fn evidence(inst: &Instance, out: &str) -> String {
    format!(
        "the loop said:\n{out}\njournal:\n{}\nsubmitted: {:?}\nkeys: {:?}\nthe worker's \
         events: {:?}",
        journal(inst),
        std::fs::read_to_string(inst.tmp.join("submits.log")).unwrap_or_default(),
        std::fs::read_to_string(inst.tmp.join("keys.log")).unwrap_or_default(),
        record(&inst.tmp.join("events")),
    )
}

/// The screens the worker draws read as the policy needs them to: the goal
/// end idle with Claude Code's suggestion at column 2; the 529 end idle.
#[test]
fn the_drawn_ends_read_as_the_policy_reads_them() {
    let g = goal_end();
    assert_eq!(worker_phase(&g), Phase::Idle, "{g:#?}");
    assert_eq!(worker_phase(&screen(END_529)), Phase::Idle);
    assert_eq!(composer_text(&g).as_deref(), Some("keep going"));
    assert!(
        is_placeholder(&g, 2),
        "the dim suggestion, the cursor at column 2"
    );
    assert!(
        !is_placeholder(&g, 12),
        "the control: filled, the cursor after it"
    );
    // The control: the measured screen, a workflow still running, is busy.
    assert_eq!(worker_phase(&screen(GOAL_ACTIVE_SUGGESTION)), Phase::Busy);
}

/// THE SUGGESTION, live: exactly one continuation reaches the worker, by
/// the accept key and a fenced Enter; the decision the next turn ends on is
/// escalated and nothing more is typed.
#[test]
fn the_suggestion_is_accepted_once_and_the_decision_after_it_is_escalated() {
    let Some(inst) = boot("s") else { return };
    let sid = boot_session(&inst);
    let (keys, subs, _events) = start_worker(&inst, &sid, "goal");
    let sup = Supervisor::start(&inst, &sid, TurnEndTiming::default());
    // A person's line: the turn runs, then ends on the suggestion.
    type_line(&inst, &sid, "go");
    let sup = sup.saw(&inst, &sid, "I.need.your.decision");
    let meta = {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let m = ctl_ok(&inst, &[&format!("@{sid}"), "meta"]);
            if m.contains("need%20your%20decision")
                || m.contains("need your decision")
                || Instant::now() >= deadline
            {
                break m;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    };
    let out = sup.stop();
    let submitted = lines_when(&subs, 2, Duration::from_secs(1));
    assert_eq!(
        submitted,
        ["SUBMIT:go", "SUBMIT:keep going"],
        "exactly one continuation; the loop said:\n{out}"
    );
    let log = std::fs::read(&keys).expect("the key log");
    let rights = log.windows(3).filter(|w| *w == b"\x1b[C").count();
    assert_eq!(rights, 1, "one accept key; the loop said:\n{out}");
    assert!(
        out.contains("rule=continue-suggestion@v1 keep going"),
        "the loop said:\n{out}"
    );
    assert!(
        meta.contains("attention_owner=supervisor"),
        "escalated under the supervisor's key: {meta}\nthe loop said:\n{out}"
    );
}

/// THE SUGGESTION TEST'S FLAKE, forced (2026-09-28, load 35-41: "`I.need.
/// your.decision` never reached the screen: OK timeout"): the loop's thread
/// takes its first look 5 s late — longer than typing `go` and the worker's
/// whole 2 s turn — and still watches the turn it judges, because a test
/// types `go` only once the loop has read the screen it starts on
/// ([`Supervisor::looked`]): the suggestion is accepted once and the
/// decision after it escalated. The control: the first point the loop read
/// is the ready screen, from before `go`.
#[test]
fn a_loop_slow_to_take_its_first_look_still_watches_the_turn_it_judges() {
    let Some(inst) = boot("f") else { return };
    let sid = boot_session(&inst);
    let (_keys, subs, _events) = start_worker(&inst, &sid, "goal");
    let sup = Supervisor::start_forced(
        &inst,
        &sid,
        TurnEndTiming::default(),
        no_answers(),
        Forcing {
            late: Duration::from_secs(5),
            ..Forcing::default()
        },
    );
    type_line(&inst, &sid, "go");
    let sup = sup.saw(&inst, &sid, "I.need.your.decision");
    let out = sup.stop();
    let said = evidence(&inst, &out);
    let journal = journal(&inst);
    let first = journal
        .lines()
        .find(|l| l.contains(r#""kind":"event""#))
        .unwrap_or_default();
    assert!(
        first.contains("Ready for the next stage"),
        "the loop's first point: {first}\n{said}"
    );
    let submitted = lines_when(&subs, 2, Duration::from_secs(1));
    assert_eq!(submitted, ["SUBMIT:go", "SUBMIT:keep going"], "{said}");
    assert!(
        out.contains("rule=continue-suggestion@v1 keep going"),
        "{said}"
    );
}

/// A LOOP STALLED IN THE MIDDLE OF A TURN, forced: its first read after
/// each line the worker takes — `go`, then the continuation — is held until
/// 3 s after the worker drew that turn's busy screen, past the 2 s the
/// worker once spent there. The loop judges a turn by the busy work it READ
/// (`TurnEndState::observe`): a turn whose busy screen came and went while
/// its read was held reads as no work at all — the goal end as the first
/// point's back-off (2 min), the decision after the continuation as a
/// continuation not yet taken (`take_within`, 30 s) — and neither the
/// continuation nor the escalation comes in time. Still, the suggestion is
/// accepted once and the decision after it escalated, because a busy spell
/// the loop watches lasts until the loop has read it (`FAKE_WORKER`'s
/// `work`). The control: each turn's end was drawn after the held read was
/// let go, so the busy screen was still up when the loop read it.
#[test]
fn a_loop_stalled_in_a_turn_still_reads_the_work_it_judges() {
    let Some(inst) = boot("h") else { return };
    let sid = boot_session(&inst);
    let (_keys, subs, events) = start_worker(&inst, &sid, "goal");
    let sup = Supervisor::start_forced(
        &inst,
        &sid,
        TurnEndTiming::default(),
        no_answers(),
        Forcing {
            held_after: vec![1, 2],
            hold: Duration::from_secs(3),
            ..Forcing::default()
        },
    );
    type_line(&inst, &sid, "go");
    let sup = sup.saw(&inst, &sid, "I.need.your.decision");
    let meta = meta_when(&inst, &sid, |m| m.contains("attention_owner=supervisor"));
    let released = sup.released();
    let out = sup.stop();
    let said = evidence(&inst, &out);
    let submitted = lines_when(&subs, 2, Duration::from_secs(1));
    assert_eq!(submitted, ["SUBMIT:go", "SUBMIT:keep going"], "{said}");
    assert!(
        out.contains("rule=continue-suggestion@v1 keep going"),
        "{said}"
    );
    assert!(
        meta.contains("attention_owner=supervisor"),
        "escalated under the supervisor's key: {meta}\n{said}"
    );
    let ends = [drawn(&events, "goal"), drawn(&events, "stop")];
    assert_eq!(released.len(), 2, "both reads held: {released:?}\n{said}");
    for (end, let_go) in ends.iter().zip(&released) {
        assert!(
            end > let_go,
            "the turn ended at {end:?}, before the held read went out at {let_go:?}\n{said}"
        );
    }
}

/// THE 529, live: nothing before the backoff (1 s here), then ONE retry
/// quoting the vendor's line — never `keep going` — through the fenced
/// write, echoed into the worker's composer and submitted whole; the
/// decision after it is escalated. The backoff is measured as the loop
/// waited it ([`waited_at`]: from its review of the wall to the retry's
/// first key), so it binds: with the product's ladder at zero the retry's
/// first key came 2 ms after the review (2026-09-28), where the wall's
/// drawing to the retry's submission — what this measured before — took
/// 2.5 s with no backoff at all, and passed.
#[test]
fn a_529_end_is_continued_once_after_its_backoff() {
    let Some(inst) = boot("r") else { return };
    let sid = boot_session(&inst);
    let (_keys, subs, events) = start_worker(&inst, &sid, "e529");
    let backoff = Duration::from_secs(1);
    let sup = Supervisor::start(
        &inst,
        &sid,
        TurnEndTiming {
            retry_backoff: vec![backoff; 3],
            ..TurnEndTiming::default()
        },
    );
    type_line(&inst, &sid, "go");
    // The wall is continued a second after it is drawn and then replaced:
    // the worker's record, never a screen watcher armed after the fact, and
    // its stamps, never the test's clock read when it looked ([`stamps_when`]).
    let wall_at = drawn(&events, "e529");
    let submitted = lines_when(&subs, 2, Duration::from_secs(20));
    let (continued_after, reviewed) = waited_at(&inst, &events, "e529", 1, wall_at);
    let sup = sup.saw(&inst, &sid, "I.need.your.decision");
    let out = sup.stop();
    const QUOTED: &str = "Claude Code reported \"API Error: 529 Overloaded. This is a \
                          server-side issue, usually temporary — try again in a moment.";
    const CARRY_ON: &str = "Carry on from where you stopped; if the result of your last step \
                            is missing, check whether it ran before you repeat it.";
    assert_eq!(submitted.len(), 2, "the loop said:\n{out}");
    assert_eq!(submitted[0], "SUBMIT:go", "the loop said:\n{out}");
    assert!(
        submitted[1].starts_with(&format!("SUBMIT:{QUOTED}")) && submitted[1].ends_with(CARRY_ON),
        "the vendor's line quoted, whole: {:?}\nthe loop said:\n{out}",
        submitted[1]
    );
    assert!(
        continued_after >= backoff - Duration::from_millis(100),
        "typed {continued_after:?} after the loop reviewed the wall ({reviewed}), before its \
         {backoff:?} backoff; the loop said:\n{out}"
    );
    assert!(
        out.contains(&format!("rule=api-retry@v1 {QUOTED}")),
        "{out}"
    );
    assert!(!out.contains("rule=api-retry@v1 keep going"), "{out}");
    let all = lines_when(&subs, 3, Duration::from_millis(500));
    assert_eq!(
        all.len(),
        2,
        "nothing typed into the decision: {all:?}\n{out}"
    );
}

/// The standing rules the narrow case rides: with them the continuation is
/// 150 characters — `❯ ` and it fill two 64-column rows and 24 cells of a
/// third, fewer than the 40-character tail the old guard named.
const RULES: &str = "stay on the lane branch, never push to main,\nand run the lane's tests \
                     before every commit, and keep the ledger rows exact";

/// A LONG CONTINUATION IN A NARROW COMPOSER, live (module header): the
/// continuation with its standing rules wraps over three composer rows, is
/// read back whole, and is submitted ONCE; the decision after it is
/// escalated. Negative control: a composer that shows a pasted-text
/// placeholder instead of the text gets no Enter — nothing submitted — and
/// the text left typed is escalated.
#[test]
fn a_long_continuation_in_a_narrow_composer_is_submitted_once() {
    let want = format!("keep going (standing rules: {})", RULES.replace('\n', " "));
    assert_eq!((2 + want.chars().count()) % 64, 24, "the last row's cells");
    for mangle in [false, true] {
        let tag = if mangle { "m" } else { "n" };
        let Some(inst) = boot_with(tag, "64") else {
            return;
        };
        let sid = boot_session(&inst);
        let rules = inst.tmp.join("rules.txt");
        std::fs::write(&rules, RULES).expect("the rules file");
        let (_keys, subs, events) = start_worker_with(&inst, &sid, "done", "64", mangle);
        let sup = Supervisor::start_with(
            &inst,
            &sid,
            TurnEndTiming::default(),
            SupervisorConfig {
                rules_file: Some(rules),
                ..no_answers()
            },
        );
        type_line(&inst, &sid, "go");
        // Not the screen: continued (`!mangle`), it is gone ~2.5 s after it
        // is drawn, before a late watcher is armed ([`stamps_when`]).
        drawn(&events, "done");
        if !mangle {
            let sup = sup.saw(&inst, &sid, "I.need.your.decision");
            let out = sup.stop();
            let submitted = lines_when(&subs, 2, Duration::from_secs(1));
            assert_eq!(
                submitted,
                ["SUBMIT:go".to_string(), format!("SUBMIT:{want}")],
                "the loop said:\n{out}"
            );
            assert!(
                out.contains("rule=continue@v1 keep going (standing rules:"),
                "{out}"
            );
            continue;
        }
        // The control: the attention says what was left typed.
        let meta = {
            let deadline = Instant::now() + Duration::from_secs(20);
            loop {
                let m = ctl_ok(&inst, &[&format!("@{sid}"), "meta"]);
                if m.contains("not%20submitted")
                    || m.contains("not submitted")
                    || Instant::now() >= deadline
                {
                    break m;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        };
        let out = sup.stop();
        let submitted = lines_when(&subs, 2, Duration::from_millis(500));
        assert_eq!(submitted, ["SUBMIT:go"], "no Enter; the loop said:\n{out}");
        assert!(
            meta.contains("not%20submitted") || meta.contains("not submitted"),
            "escalated: {meta}\nthe loop said:\n{out}"
        );
        assert!(!out.contains("CONTINUED"), "{out}");
    }
}

/// THE HARNESS'S OWN RACE, forced (the narrow case's flake of 2026-09-27:
/// `Fixed.the.parser` never reached the screen, `OK timeout`): a test that
/// looks for the end of turn only AFTER the loop has continued it — here,
/// deterministically, once the continuation is submitted and the next turn
/// has ended — still finds it, in the worker's record ([`stamps_when`]),
/// drawn before the continuation was taken. The control is the wait the
/// tests used before: a screen watcher armed now does not see it (so the
/// losing interleaving was really reached).
#[test]
fn a_screen_the_loop_replaced_is_still_found_by_a_test_that_looks_late() {
    let Some(inst) = boot("l") else { return };
    let sid = boot_session(&inst);
    let (_keys, subs, events) = start_worker(&inst, &sid, "done");
    let sup = Supervisor::start(&inst, &sid, TurnEndTiming::default());
    type_line(&inst, &sid, "go");
    // The losing interleaving, forced by waiting for what replaces the end
    // of turn: its continuation taken and the turn after it ended.
    let took = stamps_when(&events, "submit", 2, Duration::from_secs(30));
    let stop_at = drawn(&events, "stop");
    let missed = awaited_for(&inst, &sid, "Fixed.the.parser", 1000);
    let submitted = lines_when(&subs, 2, Duration::from_secs(1));
    let out = sup.stop();
    assert_eq!(
        took.len(),
        2,
        "go and one continuation; the loop said:\n{out}"
    );
    assert_eq!(submitted[0], "SUBMIT:go", "the loop said:\n{out}");
    assert!(
        missed.is_err(),
        "the control: the end of turn is gone from the screen, so a watcher armed \
         now cannot see it; the loop said:\n{out}"
    );
    let done_at = drawn(&events, "done");
    assert!(
        took[0] <= done_at && done_at <= took[1] && took[1] <= stop_at,
        "drawn after `go` and before the continuation: {took:?} {done_at:?} {stop_at:?}"
    );
}

/// FULL POWER, a WRITE box (the measured 2.1.280 `touch x` box): its
/// one-shot allow, `1`, reaches the worker once — fenced on the judged read
/// and guarded on the command's row — and the loop says `APPROVED`.
/// NEGATIVE CONTROL: under the owner's `approve = "safe"` the same box is
/// escalated (the keyed attention) and nothing is pressed.
#[test]
fn a_write_box_gets_its_one_shot_allow_under_full_power() {
    for approve in ["all", "safe"] {
        let Some(inst) = boot(if approve == "all" { "w" } else { "v" }) else {
            return;
        };
        let sid = boot_session(&inst);
        let (_keys, subs, events) = start_worker(&inst, &sid, "box");
        let mut policy = SupervisorConfig::default();
        policy.set("approve", approve).expect("a level");
        let sup = Supervisor::start_with(&inst, &sid, TurnEndTiming::default(), policy);
        type_line(&inst, &sid, "go");
        // Not the screen: answered (`all`), the box is gone once the worker
        // reads its `1` ([`stamps_when`]).
        drawn(&events, "box");
        if approve == "all" {
            let submitted = lines_when(&subs, 2, Duration::from_secs(20));
            let out = sup.stop();
            assert_eq!(
                submitted[..2],
                ["SUBMIT:go", "PRESS:1"],
                "the loop said:\n{out}"
            );
            assert!(out.contains("APPROVED seq="), "{out}");
            continue;
        }
        let meta = meta_when(&inst, &sid, |m| m.contains("attention_owner=supervisor"));
        let out = sup.stop();
        assert!(
            meta.contains("attention_owner=supervisor"),
            "escalated: {meta}\nthe loop said:\n{out}"
        );
        let submitted = lines_when(&subs, 2, Duration::from_millis(500));
        assert_eq!(
            submitted,
            ["SUBMIT:go"],
            "nothing pressed; the loop said:\n{out}"
        );
    }
}

/// FULL POWER, a request for a decision: ANSWERED — `answer_text` typed
/// through the fenced write and submitted once — and nothing escalated.
/// (The negative control, the answers off, is every test above: there the
/// same decision is escalated and never typed into.)
#[test]
fn a_decision_is_answered_with_the_answer_text() {
    let Some(inst) = boot("a") else { return };
    let sid = boot_session(&inst);
    let (_keys, subs, _events) = start_worker(&inst, &sid, "stop");
    // The owner's text, short (the fake worker echoes one byte at a time).
    let answer = "Decide yourself and keep going.".to_string();
    let sup = Supervisor::start_with(
        &inst,
        &sid,
        TurnEndTiming::default(),
        SupervisorConfig {
            answer_text: answer.clone(),
            ..SupervisorConfig::default()
        },
    );
    type_line(&inst, &sid, "go");
    let sup = sup.saw(&inst, &sid, "I.need.your.decision");
    let submitted = lines_when(&subs, 2, Duration::from_secs(20));
    let meta = ctl_ok(&inst, &[&format!("@{sid}"), "meta"]);
    let out = sup.stop();
    assert_eq!(
        submitted[..2],
        ["SUBMIT:go".to_string(), format!("SUBMIT:{answer}")],
        "the loop said:\n{out}"
    );
    assert!(out.contains("rule=answer@v1"), "{out}");
    assert!(
        !meta.contains("attention_owner=supervisor"),
        "{meta}\n{out}"
    );
}

/// FULL POWER, a worker whose every turn is short (`min_work` an hour, the
/// injected clock): continued for ever, never escalated as done, each
/// continuation after a LONGER wait than the one before, up to the cap,
/// where it holds. The loop starts on the worker's first turn, busy: the
/// continuations after it wait 1 s, 2 s, 4 s (the cap) and 4 s again — the
/// back-offs the loop journals, and no continuation earlier than its own.
#[test]
fn a_short_turn_loop_backs_off_growing_and_never_escalates() {
    let Some(inst) = boot("b") else { return };
    let sid = boot_session(&inst);
    let (_keys, _subs, events) = start_looping_worker(&inst, &sid, "done");
    let sec = Duration::from_secs(1);
    type_line(&inst, &sid, "go");
    // `go` taken: the loop must not start on the ready screen. Not the busy
    // screen itself — it lasts the worker's 2 s turn ([`stamps_when`]): drawn
    // before the loop's connection is made, it waits for no read of it
    // ([`FAKE_WORKER`]), so the loop starts on it or on its end. Every spell
    // after it, a continuation's, lasts until the loop has read it.
    drawn(&events, "busy");
    let sup = Supervisor::start_with(
        &inst,
        &sid,
        TurnEndTiming {
            min_work: 3600 * sec,
            short_backoff: sec,
            short_backoff_max: 4 * sec,
            ..TurnEndTiming::default()
        },
        SupervisorConfig::default(),
    );
    // Each line as the worker took it (its stamps): a test that polled late
    // would bunch two arrivals and shorten the gap between them.
    let at = stamps_when(&events, "submit", 6, Duration::from_secs(90));
    let meta = ctl_ok(&inst, &[&format!("@{sid}"), "meta"]);
    let out = format!("{}\njournal:\n{}", sup.stop(), journal(&inst));
    assert_eq!(
        at.len(),
        6,
        "`go` and five continuations; the loop said:\n{out}"
    );
    // The back-offs, as the loop waited them out: doubling, then held.
    let waits: Vec<u64> = out
        .lines()
        .filter_map(|l| l.split(" short turn(s) in a row: ").nth(1))
        .filter_map(|rest| rest.split(" s back-off").next()?.parse().ok())
        .collect();
    assert_eq!(waits[..5], [1, 2, 4, 4, 4], "{out}");
    // Each back-off as the loop waited it ([`waited_at`]): from its review
    // of the end of turn to the continuation's first key. The gaps below
    // hold the loop's latency, the typing and the settle too: with every
    // back-off quartered they still passed on the 2 s one (2026-09-28), and
    // the first continuation's they do not measure at all.
    let ends = stamps_when(&events, "draw-done", 5, Duration::ZERO);
    for (n, (&drawn, wait)) in ends.iter().zip(&waits[..5]).enumerate() {
        let (waited, reviewed) = waited_at(&inst, &events, "done", n + 1, drawn);
        assert!(
            waited >= Duration::from_secs(*wait) - Duration::from_millis(300),
            "end of turn {}: typed {waited:?} after the loop reviewed it ({reviewed}), \
             before its {wait} s back-off\n{out}",
            n + 1
        );
    }
    // No continuation came before its back-off: between two, the worker's
    // 2 s turn and at least the wait.
    let gaps: Vec<Duration> = at[1..].windows(2).map(|w| between(w[0], w[1])).collect();
    for (gap, wait) in gaps.iter().zip(&waits[1..]) {
        assert!(
            *gap >= Duration::from_secs(2 + wait) - Duration::from_millis(300),
            "{gap:?} < 2 s + {wait} s: {gaps:?}\n{out}"
        );
    }
    assert!(
        !meta.contains("attention_owner=supervisor"),
        "{meta}\n{out}"
    );
}

/// A PERSON'S KEYSTROKE, live: a turn a person stopped with Esc (the
/// vendor's `Interrupted · What should Claude do instead?`) is held for
/// `human_grace_s` (3 s here) — nothing typed, nothing escalated — and then,
/// nobody having come back, continued. The hold is measured as the loop
/// kept it ([`waited_at`]), so it binds the grace: with the product's grace
/// removed the continuation's first key came 2 ms after the review, with it
/// quartered 761 ms (2026-09-28). (A keystroke through a window, the
/// server's `human_ms=`, needs a window this headless instance does not
/// have: the decider's and the loop's tests pin that one.)
#[test]
fn an_esc_interrupt_holds_the_loop_for_the_grace_then_continues() {
    let Some(inst) = boot("i") else { return };
    let sid = boot_session(&inst);
    let (_keys, subs, events) = start_worker(&inst, &sid, "interrupted");
    let grace = Duration::from_secs(3);
    // The worker's 2 s turn counts as real work here: the point is held for
    // the person, not backed off as a short turn.
    let sup = Supervisor::start_with(
        &inst,
        &sid,
        TurnEndTiming {
            min_work: Duration::from_secs(1),
            ..TurnEndTiming::default()
        },
        SupervisorConfig {
            human_grace_s: 3,
            ..SupervisorConfig::default()
        },
    );
    type_line(&inst, &sid, "go");
    // The stopped turn is held for the grace, then continued and replaced:
    // the worker's record and stamps ([`stamps_when`]), so a test that looked
    // late neither misses the screen nor shortens the hold it measures.
    let stopped_at = drawn(&events, "interrupted");
    let submitted = lines_when(&subs, 2, Duration::from_secs(20));
    let took = stamps_when(&events, "submit", 2, Duration::from_secs(1));
    let early: Vec<String> = submitted
        .iter()
        .zip(&took)
        .filter(|&(_, &at)| at < stopped_at + grace - Duration::from_millis(700))
        .map(|(line, _)| line.clone())
        .collect();
    // The hold as the loop kept it ([`waited_at`]): from its review of the
    // stopped turn to the continuation's first key.
    let (after, reviewed) = waited_at(&inst, &events, "interrupted", 1, stopped_at);
    let meta = ctl_ok(&inst, &[&format!("@{sid}"), "meta"]);
    let out = sup.stop();
    assert_eq!(
        submitted[..2],
        ["SUBMIT:go", "SUBMIT:keep going"],
        "the loop said:\n{out}"
    );
    assert!(
        after >= grace - Duration::from_millis(200),
        "typed {after:?} after the loop reviewed the stopped turn ({reviewed}), before its \
         {grace:?} grace\n{out}"
    );
    assert_eq!(
        early,
        ["SUBMIT:go"],
        "held for the grace; the loop said:\n{out}"
    );
    let journal = journal(&inst);
    assert!(
        journal.contains("a person stopped the turn with Esc"),
        "{journal}\n{out}"
    );
    assert!(
        !meta.contains("attention_owner=supervisor"),
        "{meta}\n{out}"
    );
}

/// A DRAFT LEFT STANDING, live (lane P's review: text in the composer
/// stopped a fully automatic session for ever, nobody told): text typed
/// into the worker's composer without Enter, a character every 250 ms for
/// longer than the grace (3 s here) — a person writing, as the loop sees
/// it: this headless server says nothing of a person, so the draft's own
/// changes are the clock — is never submitted or typed into while it
/// changes; once nobody has touched it for the grace it is SENT as it
/// stands, once, by a fenced Enter alone (`CONTINUED … rule=continue@v1
/// <the draft>`), and nothing is escalated.
#[test]
fn a_draft_left_standing_is_sent_once_nobody_has_touched_it_for_the_grace() {
    let Some(inst) = boot("d") else { return };
    let sid = boot_session(&inst);
    let (_keys, subs, _events) = start_worker(&inst, &sid, "done");
    let grace = Duration::from_secs(3);
    let sup = Supervisor::start_with(
        &inst,
        &sid,
        TurnEndTiming {
            short_backoff: Duration::from_secs(1),
            ..TurnEndTiming::default()
        },
        SupervisorConfig {
            human_grace_s: 3,
            ..SupervisorConfig::default()
        },
    );
    let draft = "also check the lexer";
    let typing = Instant::now();
    let at = format!("@{sid}");
    // Stamped BEFORE each key is sent, so the reference is never later than
    // the draft's true last change. It was stamped after the last `aterm ctl`
    // exited plus a 250 ms nap, a reference that lags the change the loop's
    // grace counts from by that process's exit, and under load that lag ate
    // the 300 ms tolerance below (the load-sensitive test audit of 2026-09-27).
    let mut last_key = typing;
    for c in draft.chars() {
        last_key = Instant::now();
        // A lone space is no `send` text (it is trimmed): its key instead.
        if c == ' ' {
            ctl_ok(&inst, &[&at, "key", "space"]);
        } else {
            ctl_ok(&inst, &[&at, "send", "--", &c.to_string()]);
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    let typed = typing.elapsed();
    let during = lines_when(&subs, 1, Duration::ZERO);
    let submitted = lines_when(&subs, 1, Duration::from_secs(20));
    let after = last_key.elapsed();
    let meta = ctl_ok(&inst, &[&format!("@{sid}"), "meta"]);
    let out = sup.stop();
    assert!(typed > grace, "typed for {typed:?}");
    assert!(
        during.is_empty(),
        "sent while it changed: {during:?}\n{out}"
    );
    assert_eq!(
        submitted.first().map(String::as_str),
        Some(format!("SUBMIT:{draft}").as_str()),
        "the loop said:\n{out}"
    );
    assert!(
        after >= grace - Duration::from_millis(300),
        "sent {after:?} after the last key\n{out}"
    );
    assert!(out.contains(&format!("rule=continue@v1 {draft}")), "{out}");
    assert!(
        !meta.contains("attention_owner=supervisor"),
        "{meta}\n{out}"
    );
}
