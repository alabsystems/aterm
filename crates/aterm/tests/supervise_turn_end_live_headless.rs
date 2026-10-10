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
//!   retry — the wall named in aterm's words (`Claude Code reported an API
//!   error (overloaded). Carry on from where you stopped; …`), never the
//!   screen's words (ruling 2026-10-08) and never the rote `keep going` (the
//!   outage of 2026-09-27) — through the fenced write (`send if-gen=` on the
//!   judged read, then a fenced Enter once the composer shows it) — once —
//!   and not before the backoff has run.
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

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
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

use crate::headless_boot;

use headless_boot::{Instance, ctl, log_tail, type_line};

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

/// How long a wait for something that MUST happen — a screen, a worker's
/// record, a submitted line — may take before it is read as a hang (AGENTS.md:
/// a hang detector is a minute, never a latency budget).
const HANG: Duration = aterm_spec::wait::MUST_HAPPEN;
/// [`HANG`] as a server-side `await`'s bound.
const HANG_MS: &str = "timeout=60000";
/// Wide enough that no drawn row wraps (the 529 notice is 152 columns).
const COLUMNS: &str = "170";

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
    let out = ctl(inst, &[&format!("@{sid}"), "await", "match", re, HANG_MS]);
    let reply = String::from_utf8_lossy(&out.stdout).into_owned();
    if out.status.success() && reply.starts_with("OK") && !reply.starts_with("OK timeout") {
        return Ok(());
    }
    Err(format!(
        "{reply:?} stderr={:?}",
        String::from_utf8_lossy(&out.stderr)
    ))
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
/// waiting up to a minute; panics naming what it did draw.
fn drawn(events: &Path, screen: &str) -> SystemTime {
    if let Some(&at) = stamps_when(
        events,
        &format!("draw-{screen}"),
        1,
        Duration::from_secs(60),
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
    let key = first_key_after(events, screen, n, HANG);
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

/// The measured weekly wall of 2026-09-28 (tab 1: the wall's
/// gutter block, the `/goal` echo, a done row counting `1 shell still
/// running`, the bypass footer's `· 1 shell ·`, Claude's dim suggestion in the
/// composer), its reset a few SECONDS off (`resets in 5s`, the span
/// grammar the loop reads) so a live test can wait it out. `live`: a new turn
/// runs under it (a spinner status row, `esc to interrupt` on the mode row) —
/// the negative control, busy.
fn wall_behind_a_shell(live: bool) -> Vec<String> {
    wall_behind_a_shell_in(5, live)
}

/// [`wall_behind_a_shell`] with its reset `secs` seconds off.
fn wall_behind_a_shell_in(secs: u64, live: bool) -> Vec<String> {
    const WALL: &str = include_str!(
        "../../aterm-phase/src/fixtures/claude-2.1.283-wall-weekly-goal-paused-shell-measured.txt"
    );
    let reset = format!("resets in {secs}s");
    screen(WALL)
        .into_iter()
        .map(|r| r.replace("resets 4pm (America/Los_Angeles)", &reset))
        .map(|r| {
            if live && r.starts_with("✻ Sautéed") {
                "✻ Cooking… (12s · ↓ 1.1k tokens · esc to interrupt)".to_string()
            } else if live && r.contains("bypass permissions on") {
                "  ⏵⏵ bypass permissions on · esc to interrupt · 1 shell · ← for agents".to_string()
            } else {
                r
            }
        })
        .collect()
}

/// The text a person left in the composer at the wall ([`wall_with_a_draft`]).
const DRAFT: &str = "and then merge when it is green";

/// [`wall_behind_a_shell`] with a person's DRAFT standing in the composer: the
/// caret row holds `DRAFT`, drawn plain, not as Claude's dim suggestion.
fn wall_with_a_draft() -> Vec<String> {
    let mut rows = wall_behind_a_shell(false);
    let c = caret(&rows);
    rows[c] = format!("❯ {DRAFT}");
    rows
}

/// [`wall_behind_a_shell`] with the wall's rows scrolled out of view (the owner
/// reading back up the transcript: Claude Code's fullscreen view keeps the
/// status row, the composer and the footer where they are): the workflow
/// notice, the gutter block and the echo are gone, the done row and the
/// footer still count `1 shell`. The screen reads `busy` with no wall.
fn wall_scrolled_out_of_view() -> Vec<String> {
    let mut rows = wall_behind_a_shell(false);
    let from = rows
        .iter()
        .position(|r| r.starts_with("⏺ Dynamic workflow"))
        .expect("the workflow notice");
    let to = rows
        .iter()
        .position(|r| r.starts_with("⏺ Goal paused"))
        .expect("the echo");
    rows.drain(from..=to);
    rows
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
/// act failed.
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
      if [ "$n" = 1 ] || [ "$mode" = loop ]; then
        # `wallscroll`: the wall (its reset 12 s off), then, six seconds later,
        # the same screen with the wall's rows scrolled out of view (the owner
        # reading back up): six seconds for a loaded machine to look at it.
        if [ "$after" = wallscroll ]; then draw wallslow; sleep 6; draw wallscrolled; else draw "$after"; fi
      else draw stop; fi
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
        ("wall", wall_behind_a_shell(false)),
        // The same wall on a grid drawn one row high: the cursor on the row
        // BELOW the prompt, as tab 1's screen sat after its flap of 2026-09-28.
        ("walldisplaced", wall_behind_a_shell(false)),
        ("wallspin", wall_behind_a_shell(true)),
        // The wall the scroll test draws: its reset 12 s off, so the loop has
        // seconds, not a second, to look at it before it is scrolled away.
        ("wallslow", wall_behind_a_shell_in(12, false)),
        ("wallscrolled", wall_scrolled_out_of_view()),
        // The wall with a person's draft in the composer (not dim: typed).
        ("walldraft", wall_with_a_draft()),
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
        if name == "goal"
            || name == "wall"
            || name == "walldisplaced"
            || name == "wallslow"
            || name == "wallscrolled"
        {
            // The suggestion DIM, as Claude Code draws it: the loop reads the
            // `cell` at column 2 before it takes one row of text there for the
            // placeholder, and a plain one is a person's draft, caret homed.
            // (The measured wall's caret is followed by a NO-BREAK SPACE.)
            drawn[c] =
                drawn[c]
                    .replacen("❯ ", "❯ \x1b[2m", 1)
                    .replacen("❯\u{a0}", "❯\u{a0}\x1b[2m", 1)
                    + "\x1b[22m";
        }
        let cursor_row = if name == "walldisplaced" { c + 1 } else { c };
        // A draft leaves the cursor after its text.
        let cursor_col = if name == "walldraft" {
            2 + DRAFT.chars().count()
        } else {
            2
        };
        std::fs::write(
            dir.join(format!("{name}.scr")),
            render(&drawn, (cursor_row, cursor_col)),
        )
        .expect("scr");
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
}

/// The loop's connection: every request carried to the instance as it is,
/// and the worker told when the loop has read its busy screen
/// ([`FAKE_WORKER`]'s `work`).
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
    /// The last screen read showed the busy spell.
    read_busy: bool,
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

    /// [`Self::start`] under `policy`. Returns once the loop has LOOKED — its
    /// journal holds the first point it read ([`Self::looked`]).
    fn start_with(
        inst: &Instance,
        sid: &str,
        timing: TurnEndTiming,
        policy: SupervisorConfig,
    ) -> Self {
        let before = looks(&journal(inst));
        let stop = Arc::new(AtomicBool::new(false));
        let mut relay = RelayCtl::new(Endpoint::Socket(inst.sock.clone()), None);
        relay.connect().expect("the supervisor's connection");
        // From here on the worker's busy spells wait for the loop's read.
        let seen = inst.tmp.join("seen");
        std::fs::create_dir_all(&seen).expect("the busy reads' dir");
        let mut ctl = Tap {
            ctl: relay,
            events: inst.tmp.join("events"),
            seen,
            read_busy: false,
        };
        let cut = ctl.interrupter();
        let ledger = inst.tmp.join("state/drive.jsonl");
        let journal = inst.tmp.join("state/journal.jsonl");
        let sid = format!("@{sid}");
        let flag = Arc::clone(&stop);
        let handle = std::thread::spawn(move || {
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
        Self { stop, cut, handle }.looked(inst, before)
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
        let deadline = Instant::now() + Duration::from_secs(60);
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

/// The session's `meta` once it satisfies `ok`, or after a minute as it is.
fn meta_when(inst: &Instance, sid: &str, ok: impl Fn(&str) -> bool) -> String {
    let deadline = Instant::now() + Duration::from_secs(60);
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

/// Two tests of one file that boot with the same tag share ONE scratch world
/// (`headless_boot::scratch_root` names it `atte{tag}-{pid}`): one directory,
/// one socket, one fake worker, clobbered by each other whenever libtest runs
/// them side by side. The merge gate runs this binary that way, so a repeated
/// tag is a red run that reads as load (measured 2026-09-29: six of seventeen
/// failed at RUST_TEST_THREADS=6 with four tags shared, none alone). A test that
/// needs a world of its own takes a tag no other test in this file takes.
#[test]
fn no_two_tests_boot_with_the_same_tag() {
    let src = include_str!("supervise_turn_end_live_headless.rs");
    let mut seen: Vec<(String, usize)> = Vec::new();
    for (n, line) in src.lines().enumerate() {
        let line = line.trim_start();
        let boots = line.starts_with("let Some(inst) = boot(")
            || line.starts_with("let Some(inst) = boot_with(")
            || line.starts_with("let tag =");
        if !boots {
            continue;
        }
        // Every one-letter string literal on the line is a tag (`"64"` and the
        // like are columns, and longer).
        for lit in line.split('"').skip(1).step_by(2) {
            if lit.chars().count() == 1 {
                seen.push((lit.to_string(), n + 1));
            }
        }
    }
    assert!(seen.len() >= 10, "the scan read no boots: {seen:?}");
    for (i, (tag, at)) in seen.iter().enumerate() {
        if let Some((_, first)) = seen[..i].iter().find(|(t, _)| t == tag) {
            panic!(
                "tag {tag:?} boots at line {first} and again at line {at}: a world of its own needs its own tag"
            );
        }
    }
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
        let deadline = Instant::now() + Duration::from_secs(60);
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
    let submitted = lines_when(&subs, 2, HANG);
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

/// THE 529, live: nothing before the backoff (1 s here), then ONE retry
/// naming the wall — never `keep going`, never the screen's words — through the fenced
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
    let submitted = lines_when(&subs, 2, Duration::from_secs(60));
    let (continued_after, reviewed) = waited_at(&inst, &events, "e529", 1, wall_at);
    let sup = sup.saw(&inst, &sid, "I.need.your.decision");
    let out = sup.stop();
    const QUOTED: &str = "Claude Code reported an API error (overloaded).";
    const CARRY_ON: &str = "Carry on from where you stopped; if the result of your last step \
                            is missing, check whether it ran before you repeat it.";
    assert_eq!(submitted.len(), 2, "the loop said:\n{out}");
    assert_eq!(submitted[0], "SUBMIT:go", "the loop said:\n{out}");
    assert!(
        submitted[1].starts_with(&format!("SUBMIT:{QUOTED}")) && submitted[1].ends_with(CARRY_ON),
        "the wall named, whole: {:?}\nthe loop said:\n{out}",
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
/// the text left typed is escalated. The two instances share nothing, so
/// they run side by side: one after the other they were 12.7-18 s of two
/// 6-9 s halves (measured 2026-10-05).
#[test]
fn a_long_continuation_in_a_narrow_composer_is_submitted_once() {
    let want = format!("keep going (standing rules: {})", RULES.replace('\n', " "));
    assert_eq!((2 + want.chars().count()) % 64, 24, "the last row's cells");
    std::thread::scope(|scope| {
        let runs: Vec<_> = [false, true]
            .into_iter()
            .map(|mangle| {
                let want = &want;
                scope.spawn(move || narrow_composer(want, mangle))
            })
            .collect();
        for run in runs {
            run.join().expect("a run");
        }
    });
}

/// One run of [`a_long_continuation_in_a_narrow_composer_is_submitted_once`]:
/// `want` submitted whole, or — `mangle`, the control — left typed and
/// escalated.
fn narrow_composer(want: &str, mangle: bool) {
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
        let submitted = lines_when(&subs, 2, HANG);
        assert_eq!(
            submitted,
            ["SUBMIT:go".to_string(), format!("SUBMIT:{want}")],
            "the loop said:\n{out}"
        );
        assert!(
            out.contains("rule=continue@v1 keep going (standing rules:"),
            "{out}"
        );
        return;
    }
    // The control: the attention says what was left typed.
    let meta = {
        let deadline = Instant::now() + Duration::from_secs(60);
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

/// FULL POWER, a WRITE box (the measured 2.1.280 `touch x` box): its
/// one-shot allow, `1`, reaches the worker once — fenced on the judged read
/// and guarded on the command's row — and the loop says `APPROVED`.
/// NEGATIVE CONTROL: under the owner's `approve = "safe"` the same box is
/// escalated (the keyed attention) and nothing is pressed. The two instances
/// share nothing, so they run side by side (one after the other, 9.7-15 s).
#[test]
fn a_write_box_gets_its_one_shot_allow_under_full_power() {
    std::thread::scope(|scope| {
        let runs: Vec<_> = ["all", "safe"]
            .into_iter()
            .map(|approve| scope.spawn(move || write_box(approve)))
            .collect();
        for run in runs {
            run.join().expect("a run");
        }
    });
}

/// One run of [`a_write_box_gets_its_one_shot_allow_under_full_power`] under
/// `approve`.
fn write_box(approve: &str) {
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
        let submitted = lines_when(&subs, 2, Duration::from_secs(60));
        let out = sup.stop();
        assert_eq!(
            submitted[..2],
            ["SUBMIT:go", "PRESS:1"],
            "the loop said:\n{out}"
        );
        assert!(out.contains("APPROVED seq="), "{out}");
        return;
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
    let submitted = lines_when(&subs, 2, Duration::from_secs(60));
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
/// continuations after it wait 1 s, 2 s (doubled) and 2 s again (the cap,
/// where doubling would wait 4) — the back-offs the loop journals, and no
/// continuation earlier than its own. Three short turns in a row, one past
/// the two that escalated "worker reports done" until 2026-09-24. The cap
/// is the lowest that still binds after a doubling: at 4 s the loop waited
/// 1, 2, 4, 4, 4 and the test took 37 s (2026-10-01). That the ladder
/// doubles rather than steps is the conformance walk's
/// (`supervise_conformance_turn_end.rs`, at the defaults' 2, 4, 8 min).
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
            short_backoff_max: 2 * sec,
            ..TurnEndTiming::default()
        },
        SupervisorConfig::default(),
    );
    // Each line as the worker took it (its stamps): a test that polled late
    // would bunch two arrivals and shorten the gap between them.
    let at = stamps_when(&events, "submit", 4, Duration::from_secs(90));
    let meta = ctl_ok(&inst, &[&format!("@{sid}"), "meta"]);
    let out = format!("{}\njournal:\n{}", sup.stop(), journal(&inst));
    assert_eq!(
        at.len(),
        4,
        "`go` and three continuations; the loop said:\n{out}"
    );
    // The back-offs, as the loop waited them out: doubling, then held.
    let waits: Vec<u64> = out
        .lines()
        .filter_map(|l| l.split(" short turn(s) in a row: ").nth(1))
        .filter_map(|rest| rest.split(" s back-off").next()?.parse().ok())
        .collect();
    assert_eq!(waits[..3], [1, 2, 2], "{out}");
    // Each back-off as the loop waited it ([`waited_at`]): from its review
    // of the end of turn to the continuation's first key. The gaps below
    // hold the loop's latency, the typing and the settle too: with every
    // back-off quartered they still passed on a 2 s one (2026-09-28), as
    // every one they measure here is, and the first continuation's they do
    // not measure at all.
    let ends = stamps_when(&events, "draw-done", 3, Duration::ZERO);
    for (n, (&drawn, wait)) in ends.iter().zip(&waits[..3]).enumerate() {
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
    let submitted = lines_when(&subs, 2, Duration::from_secs(60));
    let took = stamps_when(&events, "submit", 2, HANG);
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

/// THE WEEKLY WALL BEHIND A BACKGROUND SHELL
/// of 2026-09-28, live. The server publishes it as a wall, not `busy`; the
/// loop waits out its reset (5 s here) and the grace (1 s), types ONE `keep
/// going` — the fenced write, echoed and submitted whole — and never before
/// the reset. Before the reader looked under background work the screen read `busy` for as
/// long as the shell ran and nothing was ever typed.
#[test]
fn a_weekly_wall_behind_a_shell_is_published_waited_out_and_continued_once() {
    let Some(inst) = boot("c") else { return };
    let sid = boot_session(&inst);
    let (_keys, subs, events) = start_worker(&inst, &sid, "wall");
    let sup = Supervisor::start(
        &inst,
        &sid,
        TurnEndTiming {
            reset_grace: Duration::from_secs(1),
            reset_jitter: Duration::ZERO,
            ..TurnEndTiming::default()
        },
    );
    type_line(&inst, &sid, "go");
    let wall_at = drawn(&events, "wall");
    // The server's verdict, at the wall: a wall, never `busy`.
    let deadline = Instant::now() + HANG;
    let mut status = String::new();
    while Instant::now() < deadline {
        status = ctl_ok(&inst, &[&format!("@{sid}"), "status"]);
        if status.contains("agent=wall:") {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let submitted = lines_when(&subs, 2, Duration::from_secs(60));
    let (continued_after, reviewed) = waited_at(&inst, &events, "wall", 1, wall_at);
    let out = sup.stop();
    assert!(
        status.contains("agent=wall:usage-weekly"),
        "published as the wall: {status}\nthe loop said:\n{out}"
    );
    assert_eq!(
        submitted,
        ["SUBMIT:go", "SUBMIT:keep going"],
        "the loop said:\n{out}"
    );
    // The loop counts a span in whole seconds from the print it read
    // (`unix_now` truncates), so the reset is 4 to 5 s after its review.
    assert!(
        continued_after >= Duration::from_secs(4),
        "typed {continued_after:?} after the loop reviewed the wall ({reviewed}); the reset is 4-5 s off\n{out}"
    );
    assert!(
        out.contains("rule=usage-resume@v1 keep going"),
        "the loop said:\n{out}"
    );
    assert!(
        journal(&inst).contains("WAITING"),
        "the wait is journaled:\n{}",
        journal(&inst)
    );
}

/// THE NEGATIVE CONTROL — the same wall, a
/// NEW turn running under it (a spinner status row, `esc to interrupt`): the
/// screen is busy, the server says `busy`, and nothing is typed however long
/// the reset has passed.
#[test]
fn the_same_wall_under_a_live_turn_is_busy_and_nothing_is_typed() {
    let Some(inst) = boot("e") else { return };
    let sid = boot_session(&inst);
    let (_keys, subs, _events) = start_worker(&inst, &sid, "wallspin");
    let sup = Supervisor::start(
        &inst,
        &sid,
        TurnEndTiming {
            reset_grace: Duration::from_secs(1),
            reset_jitter: Duration::ZERO,
            ..TurnEndTiming::default()
        },
    );
    type_line(&inst, &sid, "go");
    std::thread::sleep(Duration::from_secs(9));
    let status = ctl_ok(&inst, &[&format!("@{sid}"), "status"]);
    let out = sup.stop();
    let submitted = lines_when(&subs, 1, HANG);
    assert!(status.contains("agent=busy"), "busy: {status}\n{out}");
    assert_eq!(
        submitted,
        ["SUBMIT:go"],
        "nothing typed under a live turn\n{out}"
    );
}

/// The same wall on a grid
/// drawn one row high — the cursor on the row BELOW the prompt — is continued
/// with `keep going`, never with a bare Enter on Claude's ghost text (which the
/// fake worker logs as an empty `SUBMIT:`). The loop reads the composer where it
/// is drawn: the cursor's column is 2 and the caret row's cell is dim.
#[test]
fn a_weekly_wall_on_a_displaced_grid_is_continued_with_keep_going_not_a_bare_enter() {
    let Some(inst) = boot("g") else { return };
    let sid = boot_session(&inst);
    let (_keys, subs, _events) = start_worker(&inst, &sid, "walldisplaced");
    // The per-read draft clock still reads the cursor's row (it starts a
    // person's grace when a ghost text first shows), so the grace is short
    // here: the reset is 5 s off.
    let sup = Supervisor::start_with(
        &inst,
        &sid,
        TurnEndTiming {
            reset_grace: Duration::from_secs(1),
            reset_jitter: Duration::ZERO,
            ..TurnEndTiming::default()
        },
        SupervisorConfig {
            human_grace_s: 2,
            ..no_answers()
        },
    );
    type_line(&inst, &sid, "go");
    let submitted = lines_when(&subs, 2, Duration::from_secs(60));
    let out = sup.stop();
    assert_eq!(
        submitted,
        ["SUBMIT:go", "SUBMIT:keep going"],
        "the loop said:\n{out}"
    );
    assert!(
        out.contains("rule=usage-resume@v1 keep going"),
        "the loop said:\n{out}"
    );
}

/// A DRAFT LEFT AT THE WALL, LIVE. A person typed into the composer before the
/// limit hit and went away: at the reset and its grace the loop SUBMITS what
/// stands there under the wall's rule (a bare Enter), and never types `keep
/// going` over it. The fake worker's composer buffer is empty (the draft is
/// only drawn), so the submit it logs is an empty `SUBMIT:`.
#[test]
fn a_draft_left_at_the_wall_is_submitted_after_the_reset_not_keep_going() {
    let Some(inst) = boot("j") else { return };
    let sid = boot_session(&inst);
    let (_keys, subs, _events) = start_worker(&inst, &sid, "walldraft");
    let sup = Supervisor::start_with(
        &inst,
        &sid,
        TurnEndTiming {
            reset_grace: Duration::from_secs(1),
            reset_jitter: Duration::ZERO,
            ..TurnEndTiming::default()
        },
        SupervisorConfig {
            human_grace_s: 2,
            ..no_answers()
        },
    );
    type_line(&inst, &sid, "go");
    let submitted = lines_when(&subs, 2, Duration::from_secs(60));
    let out = sup.stop();
    assert_eq!(
        submitted,
        ["SUBMIT:go", "SUBMIT:"],
        "the draft is submitted as it stands; the loop said:\n{out}"
    );
    assert!(
        out.contains("rule=usage-resume@v1") && !out.contains("keep going"),
        "the loop said:\n{out}"
    );
}

/// THE WALL SCROLLED
/// OUT OF VIEW. The loop reads the wall behind the shell and waits for its reset
/// (12 s here); six seconds later the owner has scrolled back up the transcript,
/// the wall's rows are gone and the screen reads `busy` with no wall (the server
/// says `busy`, as it always did). The wait goes on: `keep going` is typed
/// once, at the reset and its grace, through the fenced write. Before the wall was
/// remembered, the loop waited on a screen that read
/// busy and typed nothing — the 4 h 44 m of 2026-09-28, by another door.
#[test]
fn a_weekly_wall_scrolled_out_of_view_is_still_continued_at_its_reset() {
    let Some(inst) = boot("k") else { return };
    let sid = boot_session(&inst);
    let (_keys, subs, events) = start_worker(&inst, &sid, "wallscroll");
    let sup = Supervisor::start(
        &inst,
        &sid,
        TurnEndTiming {
            reset_grace: Duration::from_secs(1),
            reset_jitter: Duration::ZERO,
            ..TurnEndTiming::default()
        },
    );
    type_line(&inst, &sid, "go");
    let wall_at = drawn(&events, "wallslow");
    let submitted = lines_when(&subs, 2, Duration::from_secs(60));
    let (continued_after, reviewed) = waited_at(&inst, &events, "wallslow", 1, wall_at);
    // The screen the loop waited on was the scrolled one, and the server read it
    // as busy (a shell still runs).
    let status = ctl_ok(&inst, &[&format!("@{sid}"), "status"]);
    let out = sup.stop();
    assert_eq!(
        submitted,
        ["SUBMIT:go", "SUBMIT:keep going"],
        "the loop said:\n{out}"
    );
    // The loop counts a span in whole seconds from the print it read
    // (`unix_now` truncates), so the reset is 11 to 12 s after its review.
    assert!(
        continued_after >= Duration::from_secs(10),
        "typed {continued_after:?} after the loop reviewed the wall ({reviewed}); the reset is 11-12 s off\n{out}"
    );
    assert!(
        out.contains("rule=usage-resume@v1 keep going"),
        "the loop said:\n{out}"
    );
    assert!(!status.is_empty());
}

/// THE UPDATE THAT APPLIES WHILE THE WALL IS SCROLLED OUT OF VIEW, live. A loop
/// reads the wall behind the shell and waits for its reset (12 s here); the owner
/// scrolls back and the wall's rows are gone. The loop is REPLACED then — an
/// update, a fault — by a new one on the same ledger and the same tab. The
/// first wrote the wall down; the second starts on a screen that reads `busy`
/// with no wall, finds the row and goes on waiting for the same reset: ONE
/// `keep going` is typed, by the second, at the reset and its grace, and the
/// first typed nothing. Before the wall was written down, the successor had
/// nothing to remember and the tab sat for hours.
#[test]
fn a_loop_that_replaces_another_at_a_scrolled_wall_goes_on_waiting_for_its_reset() {
    let Some(inst) = boot("o") else { return };
    let sid = boot_session(&inst);
    let (_keys, subs, events) = start_worker(&inst, &sid, "wallscroll");
    let timing = || TurnEndTiming {
        reset_grace: Duration::from_secs(1),
        reset_jitter: Duration::ZERO,
        ..TurnEndTiming::default()
    };
    let first = Supervisor::start(&inst, &sid, timing());
    type_line(&inst, &sid, "go");
    let wall_at = drawn(&events, "wallslow");
    // The wall scrolls away; the first loop has waited on it by then.
    let scrolled_at = drawn(&events, "wallscrolled");
    if let Ok(left) = (scrolled_at + Duration::from_millis(800)).duration_since(SystemTime::now()) {
        std::thread::sleep(left);
    }
    let ledger = std::fs::read_to_string(inst.tmp.join("state/drive.jsonl")).unwrap_or_default();
    let said_first = first.stop();
    assert!(
        ledger.contains("wall-wait@v1"),
        "the first loop wrote the wall down:\n{ledger}\n{said_first}"
    );
    // The successor: the reset is still ahead (it is 12 s after the print).
    let second = Supervisor::start(&inst, &sid, timing());
    let submitted = lines_when(&subs, 2, Duration::from_secs(60));
    let (continued_after, reviewed) = waited_at(&inst, &events, "wallslow", 1, wall_at);
    let said_second = second.stop();
    assert_eq!(
        submitted,
        ["SUBMIT:go", "SUBMIT:keep going"],
        "the successor said:\n{said_second}\nthe first said:\n{said_first}"
    );
    assert!(
        !said_first.contains("rule=usage-resume@v1"),
        "the first loop typed nothing:\n{said_first}"
    );
    assert!(
        said_second.contains("rule=usage-resume@v1 keep going"),
        "the successor said:\n{said_second}"
    );
    assert!(
        continued_after >= Duration::from_secs(10),
        "typed {continued_after:?} after the wall was reviewed ({reviewed}); the reset is 11-12 s off\n{said_second}"
    );
}
