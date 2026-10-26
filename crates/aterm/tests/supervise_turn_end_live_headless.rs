// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Live-headless check of the supervisor FULLY AUTOMATIC (aterm-agent `supervise`: the approval and turn-end policies, carried out by `Session::run_hosted`) over a real headless instance and its real control socket, through the supervisor's own persistent connection (`RelayCtl`).

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime};

use aterm_agent::supervise::config::CONTINUE_TEXT;
use aterm_agent::supervise::policy::needs::NEEDS_HINT;
use aterm_agent::supervise::policy::turn_end::TurnEndTiming;
use aterm_agent::supervise::prompt::fixtures::{
    BOX_BASH_TOUCH, END_529, GOAL_ACTIVE_SUGGESTION, composer, screen,
};
use aterm_agent::supervise::{
    Ctl, CtlReply, Endpoint, Interrupter, RelayCtl, Session, SuperviseOpts, SupervisorConfig,
};

use crate::headless_boot;

use headless_boot::{Instance, ctl, log_tail, type_line};

/// Boot one headless instance, 40 rows, [`COLUMNS`] wide.
fn boot(tag: &str) -> Instance {
    boot_with(tag, COLUMNS)
}

/// Boot one headless instance, 40 rows, `columns` wide ([`headless_boot::boot`]).
fn boot_with(tag: &str, columns: &str) -> Instance {
    static BOOTS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = BOOTS.fetch_add(1, Ordering::Relaxed);
    headless_boot::boot(
        &format!("atte{tag}{n}"),
        &["--lines", "40", "--columns", columns],
    )
}

/// One-minute hang detector for a wait that must happen.
const HANG: Duration = aterm_spec::wait::MUST_HAPPEN;
/// [`HANG`] as a server-side `await`'s bound.
const HANG_MS: &str = "timeout=60000";
/// Wide enough that no drawn row wraps (the 529 notice is 152 columns).
const COLUMNS: &str = "170";
/// The loop's approvals ledger, under the scratch root.
const LEDGER: &str = "state/drive.jsonl";

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

/// Submit `line` and its Enter in one `feed-bin` frame.
fn submit(inst: &Instance, sid: &str, line: &str) {
    let mut c = aterm_agent::RelayClient::connect_local_beside(&inst.sock)
        .unwrap_or_else(|e| panic!("connect to {}: {e}", inst.sock));
    let reply = c
        .request_frame(&format!("@{sid} feed-bin"), format!("{line}\r").as_bytes())
        .unwrap_or_else(|e| panic!("feed-bin {line:?}: no reply ({e})"));
    assert!(
        reply.starts_with("OK"),
        "feed-bin {line:?} answered {reply:?}"
    );
}

/// `await match <re>` on the session (`<re>` is one wire token, `.` for a space).
fn await_match(inst: &Instance, sid: &str, re: &str) {
    if let Err(reply) = awaited(inst, sid, re) {
        panic!("`{re}` never reached the screen: {reply}");
    }
}

/// [`await_match`] handing the reply back, bounded by [`HANG`] with no step of [`progress`] taken.
fn awaited(inst: &Instance, sid: &str, re: &str) -> Result<(), String> {
    let mut taken = progress(inst);
    loop {
        let out = ctl(inst, &[&format!("@{sid}"), "await", "match", re, HANG_MS]);
        let reply = String::from_utf8_lossy(&out.stdout).into_owned();
        if out.status.success() && reply.starts_with("OK") && !reply.starts_with("OK timeout") {
            return Ok(());
        }
        let timed_out = reply.starts_with("OK timeout");
        let now = progress(inst);
        if !timed_out || now == taken {
            let hung = if timed_out {
                format!(" (no step of the chain taken in {HANG:?})")
            } else {
                String::new()
            };
            return Err(format!(
                "{reply:?} stderr={:?}{hung}",
                String::from_utf8_lossy(&out.stderr)
            ));
        }
        taken = now;
    }
}

/// The steps a live chain has taken: worker events and loop journal steps ([`steps`]).
fn progress(inst: &Instance) -> (usize, usize) {
    (
        worker_events(&inst.tmp.join("events")).len(),
        steps(&journal(inst)),
    )
}

/// Journal kinds the hosted loop writes again on its own with no step of a chain taken.
const LOOP_OWN: [&str; 4] = ["skipped", "waiting", "reconnect", "timeout"];

/// Distinct non-[`LOOP_OWN`] lines in `journal`.
fn steps(journal: &str) -> usize {
    let mut seen = std::collections::HashSet::new();
    journal
        .lines()
        .filter_map(|l| aterm_agent::supervise::journal::JournalRecord::from_json(l).ok())
        .filter(|r| !LOOP_OWN.contains(&r.kind.as_str()))
        .filter(|r| seen.insert(r.line.clone()))
        .count()
}

/// A loop that only repeats itself takes no step ([`steps`]).
#[test]
fn a_loop_that_only_repeats_itself_takes_no_step() {
    let line = |kind: &str, text: &str| {
        format!(
            r#"{{"t":1,"sid":"s-1","kind":"{kind}","phase":"-","seq":7,"complete":null,"rows":null,"summary":"","line":"{text}","turn":null,"report":null}}"#
        )
    };
    let event = line("event", "EVENT idle seq=7 done");
    let base = steps(&event);
    assert_eq!(base, 1, "an event read is a step");
    let mut looping = event.clone();
    for n in 0..5 {
        for (kind, text) in [
            (
                "skipped",
                format!("SKIPPED seq=7 rule=continue@v1 try {n}; nothing written"),
            ),
            (
                "waiting",
                format!("WAITING seq=7 (try {n}): read and tried again in 500 ms"),
            ),
            ("reconnect", format!("RECONNECT try {n}")),
            ("timeout", format!("TIMEOUT {n}")),
            ("event", "EVENT idle seq=7 done".to_string()),
        ] {
            looping.push('\n');
            looping.push_str(&line(kind, &text));
        }
    }
    assert_eq!(
        steps(&looping),
        base,
        "the loop repeating itself: {looping}"
    );
    for (kind, text) in [
        ("event", "EVENT idle seq=8 done"),
        ("continued", "CONTINUED seq=8 rule=continue@v1 keep going"),
    ] {
        let moved = format!("{looping}\n{}", line(kind, text));
        assert_eq!(steps(&moved), base + 1, "{kind} is a step");
    }
}

/// Wait for `done` while the chain moves, failing after [`HANG`] with no step of [`progress`] taken.
fn moving<T>(inst: &Instance, mut done: impl FnMut() -> Option<T>) -> Result<T, String> {
    let mut taken = progress(inst);
    let mut since = Instant::now();
    loop {
        if let Some(answer) = done() {
            return Ok(answer);
        }
        let now = progress(inst);
        if now != taken {
            taken = now;
            since = Instant::now();
        } else if since.elapsed() >= HANG {
            return Err(format!("no step of the chain taken in {HANG:?}"));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The lines of `path` once it holds `n` of them, or when the chain hangs ([`moving`]).
fn submits_when(inst: &Instance, path: &Path, n: usize) -> Vec<String> {
    let read = || -> Vec<String> {
        std::fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    };
    moving(inst, || {
        let lines = read();
        (lines.len() >= n).then_some(lines)
    })
    .unwrap_or_else(|_| read())
}

/// The fake worker's events of `kind` (`draw-<screen>`, `submit`, `press`), their stamps in order, once `n` exist ([`moving`]).
fn stamps_when(inst: &Instance, kind: &str, n: usize) -> Vec<SystemTime> {
    let events = inst.tmp.join("events");
    let seen = || -> Vec<SystemTime> {
        worker_events(&events)
            .into_iter()
            .filter(|(_, k, _)| k == kind)
            .map(|(_, _, at)| at)
            .collect()
    };
    moving(inst, || {
        let stamps = seen();
        (stamps.len() >= n).then_some(stamps)
    })
    .unwrap_or_else(|_| seen())
}

/// When the worker first drew `screen` ([`stamps_when`]), or panic naming what it did draw.
fn drawn(inst: &Instance, screen: &str) -> SystemTime {
    if let Some(&at) = stamps_when(inst, &format!("draw-{screen}"), 1).first() {
        return at;
    }
    panic!(
        "the worker never drew `{screen}` (no step of the chain taken in {HANG:?}): it did {:?}",
        record(&inst.tmp.join("events"))
    );
}

/// The worker's whole event record in order, each event stamped in unix ms.
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

/// Every event in the worker's record ([`stamps_when`]) — sequence, kind, stamp — in order.
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

/// The lines of `path` once it holds `n` of them, or after `within`.
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

/// Rows joined by `\r\n`, with the cursor placed at 0-based (`row`, `col`).
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

/// The measured `/goal` screen at the end of its turn (`◎ /goal active` and dim `❯ keep going` kept).
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

/// The measured weekly wall behind a background shell, with its reset 5 seconds off.
fn wall_behind_a_shell() -> Vec<String> {
    wall_behind_a_shell_in(5)
}

/// [`wall_behind_a_shell`] with its reset `secs` seconds off.
fn wall_behind_a_shell_in(secs: u64) -> Vec<String> {
    const WALL: &str = include_str!(
        "../../aterm-phase/src/fixtures/claude-2.1.283-wall-weekly-goal-paused-shell-measured.txt"
    );
    let reset = format!("resets in {secs}s");
    screen(WALL)
        .into_iter()
        .map(|r| r.replace("resets 4pm (America/Los_Angeles)", &reset))
        .collect()
}

/// The text a person left in the composer at the wall ([`wall_with_a_draft`]).
const DRAFT: &str = "and then merge when it is green";

/// [`wall_behind_a_shell`] with a person's [`DRAFT`] standing in the composer.
fn wall_with_a_draft() -> Vec<String> {
    let mut rows = wall_behind_a_shell();
    let c = caret(&rows);
    rows[c] = format!("❯ {DRAFT}");
    rows
}

/// [`wall_behind_a_shell`] with the wall's rows scrolled out of view while the done row and footer still count `1 shell`.
fn wall_scrolled_out_of_view() -> Vec<String> {
    let mut rows = wall_behind_a_shell();
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

/// The decision the second turn ends on.
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

/// Fake worker shell script driven by [`start_worker_in`].
const FAKE_WORKER: &str = r#"#!/bin/sh
dir="$1"; keys="$2"; subs="$3"; after="$4"; cols="$5"; mangle="$6"; mode="$7"
events="$8"; seen="$9"; ledger="${10}"
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
# Read the complete screen and its composer metadata BEFORE changing the
# terminal. The old clear-then-cat left a process-start gap inside the redraw;
# the loop could read its suggestion before the final cursor-home sequence and
# take the text under a displaced cursor for a person's draft. One builtin
# writes the clear, screen and final cursor together. Composer redraws likewise
# use only builtins after loading their metadata. Every frame is drawn inside a
# DEC 2026 synchronized-output window, as Claude Code draws its own: one
# builtin's bytes can still reach the terminal in more than one read, and the
# loop's reads of the screen are made on whole frames, so it never reads part
# of one.
draw() {
  screen=$(cat "$dir/$1.scr"; printf .); screen="${screen%.}"
  # The box is answered by a key, never composed into: it has no composer.
  if [ -e "$dir/$1.row" ]; then
    IFS= read -r row < "$dir/$1.row"
    below=$(cat "$dir/$1.below"; printf .); below="${below%.}"
  fi
  mark "draw-$1"; cur="$1"
  printf '\033[?2026h\033[H\033[2J%s\033[?2026l' "$screen"
}
# A busy spell of $1 s. While a loop watches (its connection made "$seen"
# before this spell began, `Tap`), the seconds run from when the loop has READ
# the busy screen — the request it makes only after a read that counted the
# turn's work marks "$seen/<n>" for the spell drawn as event n — so a loop
# whose read stalls past the spell still reads it, and reads $1 s of work.
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
  printf '\033[?2026h\033[%s;1H\033[J\033[%s;1H%s\033[%s;1H❯ %s\033[?2026l' \
    "$row" "$((row + n))" "$below" "$row" "$shown"
}
# Every read that brought bytes is an event too, `key`, stamped as soon as it
# returns: the first after a screen is when what the loop typed there reached
# the worker.
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
      work 1
      if [ "$n" = 1 ] || [ "$mode" = loop ]; then
        # `wallscroll`: the wall (its reset 6 s off), then, once the loop has
        # written its wait on it down (`wall-wait@v1` in its ledger: it has
        # read the wall, however late it looked), the same screen with the
        # wall's rows scrolled out of view (the owner reading back up).
        if [ "$after" = wallscroll ]; then
          draw wallslow
          until grep -q 'wall-wait@v1' "$ledger" 2>/dev/null; do sleep 0.05; done
          draw wallscrolled
        else draw "$after"; fi
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

/// The fake worker's three logs: keys read, lines submitted, and event directory ([`stamps_when`]).
type WorkerLogs = (PathBuf, PathBuf, PathBuf);

/// Write the screens and script, start the worker as `claude` in the boot session, and wait for its ready screen.
fn start_worker(inst: &Instance, sid: &str, after: &str) -> WorkerLogs {
    start_worker_with(inst, sid, after, COLUMNS, false)
}

/// [`start_worker`], every turn ending on `after`.
fn start_looping_worker(inst: &Instance, sid: &str, after: &str) -> WorkerLogs {
    start_worker_in(inst, sid, after, COLUMNS, false, true)
}

/// [`start_worker`] for an instance `columns` wide, mangling the composer past 40 characters when `mangle`.
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
        ("wall", wall_behind_a_shell()),
        ("walldisplaced", wall_behind_a_shell()),
        ("wallslow", wall_behind_a_shell_in(6)),
        ("wallscrolled", wall_scrolled_out_of_view()),
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
            drawn[c] =
                drawn[c]
                    .replacen("❯ ", "❯ \x1b[2m", 1)
                    .replacen("❯\u{a0}", "❯\u{a0}\x1b[2m", 1)
                    + "\x1b[22m";
        }
        let cursor_row = if name == "walldisplaced" { c + 1 } else { c };
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
            "/bin/bash -c 'exec -a claude /bin/sh {} {} {} {} {after} {columns} {} {} {} {} {}'",
            script.display(),
            dir.display(),
            keys.display(),
            subs.display(),
            if mangle { "mangle" } else { "-" },
            if looping { "loop" } else { "once" },
            events.display(),
            inst.tmp.join("seen").display(),
            inst.tmp.join(LEDGER).display(),
        ),
    );
    await_match(inst, sid, "Ready.for.the.next.stage");
    (keys, subs, events)
}

/// The hosted loop on its own thread, over its own persistent connection, stopped by [`Supervisor::stop`].
struct Supervisor {
    stop: Arc<AtomicBool>,
    cut: Option<Interrupter>,
    handle: std::thread::JoinHandle<String>,
}

/// The loop's connection, notifying the worker when the loop has read its busy screen ([`FAKE_WORKER`]).
struct Tap {
    ctl: RelayCtl,
    events: PathBuf,
    seen: PathBuf,
    /// The last screen read showed the busy spell.
    read_busy: bool,
    /// Raw reads log for diagnosing fixture frames.
    reads: PathBuf,
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
        // The loop's wait for a running turn: `await log` where the host keeps the session
        // log, `await gone` on the screen where it keeps none (6H-5).
        if verb == Some("await") && matches!(words.next(), Some("log" | "gone")) && self.read_busy {
            self.saw_busy();
        }
        let reply = self.ctl.call(args);
        if matches!(verb, Some("text" | "status")) {
            use std::io::Write as _;
            if let Ok(mut file) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.reads)
            {
                let _ = writeln!(file, "{args:?}: {reply:?}");
            }
        }
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
    /// Start under [`no_answers`].
    fn start(inst: &Instance, sid: &str, timing: TurnEndTiming) -> Self {
        Self::start_with(inst, sid, timing, no_answers())
    }

    /// [`Self::start`] under `policy`, returning once the loop has journaled its first read ([`Self::looked`]).
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
        let seen = inst.tmp.join("seen");
        std::fs::create_dir_all(&seen).expect("the busy reads' dir");
        let mut ctl = Tap {
            ctl: relay,
            events: inst.tmp.join("events"),
            seen,
            read_busy: false,
            reads: inst.tmp.join("reads.log"),
        };
        let cut = ctl.interrupter();
        let ledger = inst.tmp.join(LEDGER);
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

    /// Wait until the loop has read the screen and journaled the point it shows, or panic with [`evidence`].
    fn looked(self, inst: &Instance, before: usize) -> Self {
        let looked = moving(inst, || {
            if looks(&journal(inst)) > before {
                Some(true)
            } else {
                self.handle.is_finished().then_some(false)
            }
        });
        if looked == Ok(true) {
            return self;
        }
        let out = self.stop();
        panic!(
            "the loop never journaled a point it read ({looked:?})\n{}",
            evidence(inst, &out)
        );
    }

    /// [`await_match`] for a screen that stays once drawn, stopping the loop and panicking with [`evidence`] on timeout.
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

/// The session's `meta` once it satisfies `ok`, or when the chain hangs ([`moving`]).
fn meta_when(inst: &Instance, sid: &str, ok: impl Fn(&str) -> bool) -> String {
    let meta = || ctl_ok(inst, &[&format!("@{sid}"), "meta"]);
    moving(inst, || {
        let m = meta();
        ok(&m).then_some(m)
    })
    .unwrap_or_else(|_| meta())
}

/// The loop's journal.
fn journal(inst: &Instance) -> String {
    std::fs::read_to_string(inst.tmp.join("state/journal.jsonl")).unwrap_or_default()
}

/// The loop's journal once it satisfies `ok`, or when the chain hangs ([`moving`]).
fn journal_when(inst: &Instance, ok: impl Fn(&str) -> bool) -> String {
    moving(inst, || {
        let j = journal(inst);
        ok(&j).then_some(j)
    })
    .unwrap_or_else(|_| journal(inst))
}

/// The points the loop read, as its journal holds them (`EVENT …`).
fn looks(journal: &str) -> usize {
    journal
        .lines()
        .filter(|l| l.contains(r#""kind":"event""#))
        .count()
}

/// Whether the loop said it typed `text` under `rule` (`CONTINUED … rule=<rule> <head>`).
fn continued_with(out: &str, rule: &str, text: &str) -> bool {
    let named = format!(" rule={rule} ");
    out.lines().any(|l| {
        l.starts_with("CONTINUED ")
            && l.split_once(&named)
                .is_some_and(|(_, said)| !said.is_empty() && text.starts_with(said))
    })
}

/// The worker's submit-log line for Cruise's continuation ([`CONTINUE_TEXT`]).
fn cruise_submit() -> String {
    format!("SUBMIT:{CONTINUE_TEXT}")
}

/// Diagnostic summary of the loop's output, journal, submits, keys, events, and raw reads.
fn evidence(inst: &Instance, out: &str) -> String {
    let reads = std::fs::read_to_string(inst.tmp.join("reads.log")).unwrap_or_default();
    let mut start = reads.len().saturating_sub(32 * 1024);
    while !reads.is_char_boundary(start) {
        start += 1;
    }
    format!(
        "the loop said:\n{out}\njournal:\n{}\nsubmitted: {:?}\nkeys: {:?}\nthe worker's \
         events: {:?}\nlast raw reads:\n{}",
        journal(inst),
        std::fs::read_to_string(inst.tmp.join("submits.log")).unwrap_or_default(),
        std::fs::read_to_string(inst.tmp.join("keys.log")).unwrap_or_default(),
        record(&inst.tmp.join("events")),
        &reads[start..],
    )
}

/// Cruise types its whole continuation over the worker's grey suggestion, never the accept key
/// (v6 §14.5), once, and the decision after it is escalated.
#[test]
fn the_suggestion_is_typed_over_once_and_the_decision_after_it_is_escalated() {
    let inst = boot("s");
    let sid = boot_session(&inst);
    let (keys, subs, _events) = start_worker(&inst, &sid, "goal");
    let sup = Supervisor::start(&inst, &sid, TurnEndTiming::default());
    submit(&inst, &sid, "go");
    let sup = sup.saw(&inst, &sid, "I.need.your.decision");
    let meta = meta_when(&inst, &sid, |m| {
        m.contains("need%20your%20decision") || m.contains("need your decision")
    });
    let out = sup.stop();
    let ev = evidence(&inst, &out);
    let submitted = submits_when(&inst, &subs, 2);
    assert_eq!(
        submitted,
        ["SUBMIT:go".to_string(), cruise_submit()],
        "exactly one continuation, Cruise's own words; {ev}"
    );
    let log = std::fs::read(&keys).expect("the key log");
    let rights = log.windows(3).filter(|w| *w == b"\x1b[C").count();
    assert_eq!(rights, 0, "no accept key; {ev}");
    assert!(continued_with(&out, "continue@v1", CONTINUE_TEXT), "{ev}");
    assert!(
        meta.contains("attention_owner=supervisor"),
        "escalated under the supervisor's key: {meta}\n{ev}"
    );
}

/// A 529 end is continued once after its backoff with a retry naming the wall.
#[test]
fn a_529_end_is_continued_once_after_its_backoff() {
    let inst = boot("r");
    let sid = boot_session(&inst);
    let (_keys, subs, _events) = start_worker(&inst, &sid, "e529");
    let backoff = Duration::from_secs(1);
    let sup = Supervisor::start(
        &inst,
        &sid,
        TurnEndTiming {
            retry_backoff: vec![backoff; 3],
            ..TurnEndTiming::default()
        },
    );
    submit(&inst, &sid, "go");
    drawn(&inst, "e529");
    let submitted = submits_when(&inst, &subs, 2);
    let sup = sup.saw(&inst, &sid, "I.need.your.decision");
    let out = sup.stop();
    let ev = evidence(&inst, &out);
    assert_eq!(submitted.len(), 2, "{ev}");
    assert_eq!(submitted[0], "SUBMIT:go", "{ev}");
    assert!(
        submitted[1].contains("(overloaded)") && submitted[1] != "SUBMIT:keep going",
        "the wall named: {:?}\n{ev}",
        submitted[1]
    );
    assert!(out.contains("rule=api-retry@v1 "), "{ev}");
}

/// Standing rules for the narrow-composer test (401 characters with Cruise's continuation).
const RULES: &str = "stay on the lane branch, never push to main,\nand run the lane's tests \
                     before every commit";

/// A long continuation in a narrow composer wraps across seven rows, is read back whole, and is submitted once.
#[test]
fn a_long_continuation_in_a_narrow_composer_is_submitted_once() {
    let want = format!(
        "{CONTINUE_TEXT} (standing rules: {})",
        RULES.replace('\n', " ")
    );
    let tail = (2 + want.chars().count()) % 64;
    assert!(
        (1..40).contains(&tail),
        "the last row holds fewer than 40 cells, not {tail}"
    );
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

/// One run of [`a_long_continuation_in_a_narrow_composer_is_submitted_once`].
fn narrow_composer(want: &str, mangle: bool) {
    let tag = if mangle { "m" } else { "n" };
    let inst = boot_with(tag, "64");
    let sid = boot_session(&inst);
    let rules = inst.tmp.join("rules.txt");
    std::fs::write(&rules, RULES).expect("the rules file");
    let (_keys, subs, _events) = start_worker_with(&inst, &sid, "done", "64", mangle);
    let sup = Supervisor::start_with(
        &inst,
        &sid,
        TurnEndTiming::default(),
        SupervisorConfig {
            rules_file: Some(rules),
            ..no_answers()
        },
    );
    submit(&inst, &sid, "go");
    drawn(&inst, "done");
    if !mangle {
        let sup = sup.saw(&inst, &sid, "I.need.your.decision");
        let out = sup.stop();
        let ev = evidence(&inst, &out);
        let submitted = submits_when(&inst, &subs, 2);
        assert_eq!(
            submitted,
            ["SUBMIT:go".to_string(), format!("SUBMIT:{want}")],
            "{ev}"
        );
        assert!(continued_with(&out, "continue@v1", want), "{ev}");
        return;
    }
    let meta = meta_when(&inst, &sid, |m| {
        m.contains("not%20submitted") || m.contains("not submitted")
    });
    let out = sup.stop();
    let ev = evidence(&inst, &out);
    let submitted = lines_when(&subs, 2, Duration::from_millis(500));
    assert_eq!(submitted, ["SUBMIT:go"], "no Enter; {ev}");
    assert!(
        meta.contains("not%20submitted") || meta.contains("not submitted"),
        "escalated: {meta}\n{ev}"
    );
    assert!(!out.contains("CONTINUED"), "{ev}");
}

/// A write box gets its one-shot allow (`1`) under `approve = "all"`, and is escalated under `"safe"`.
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

/// One run of [`a_write_box_gets_its_one_shot_allow_under_full_power`] under `approve`.
fn write_box(approve: &str) {
    let inst = boot(if approve == "all" { "w" } else { "v" });
    let sid = boot_session(&inst);
    let (_keys, subs, _events) = start_worker(&inst, &sid, "box");
    let mut policy = SupervisorConfig::default();
    policy.set("approve", approve).expect("a level");
    let sup = Supervisor::start_with(&inst, &sid, TurnEndTiming::default(), policy);
    submit(&inst, &sid, "go");
    drawn(&inst, "box");
    if approve == "all" {
        let submitted = submits_when(&inst, &subs, 2);
        let said = journal_when(&inst, |j| j.contains("APPROVED seq="));
        let out = sup.stop();
        let ev = evidence(&inst, &out);
        assert_eq!(
            submitted.iter().take(2).cloned().collect::<Vec<String>>(),
            ["SUBMIT:go", "PRESS:1"],
            "{ev}"
        );
        assert!(
            said.contains("APPROVED seq=") && out.contains("APPROVED seq="),
            "{out}\njournal:\n{said}"
        );
        return;
    }
    let meta = meta_when(&inst, &sid, |m| m.contains("attention_owner=supervisor"));
    let out = sup.stop();
    let ev = evidence(&inst, &out);
    assert!(
        meta.contains("attention_owner=supervisor"),
        "escalated: {meta}\n{ev}"
    );
    let submitted = lines_when(&subs, 2, Duration::from_millis(500));
    assert_eq!(submitted, ["SUBMIT:go"], "nothing pressed; {ev}");
}

/// Under full power, a decision request is answered with `answer_text` plus [`NEEDS_HINT`] and not escalated.
#[test]
fn a_decision_is_answered_with_the_answer_text() {
    let inst = boot("a");
    let sid = boot_session(&inst);
    let (_keys, subs, _events) = start_worker(&inst, &sid, "stop");
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
    submit(&inst, &sid, "go");
    let sup = sup.saw(&inst, &sid, "I.need.your.decision");
    let submitted = submits_when(&inst, &subs, 2);
    let meta = ctl_ok(&inst, &[&format!("@{sid}"), "meta"]);
    let out = sup.stop();
    let ev = evidence(&inst, &out);
    assert_eq!(
        submitted.iter().take(2).cloned().collect::<Vec<String>>(),
        [
            "SUBMIT:go".to_string(),
            format!("SUBMIT:{answer} {NEEDS_HINT}")
        ],
        "{ev}"
    );
    assert!(out.contains("rule=answer@v1"), "{ev}");
    assert!(!meta.contains("attention_owner=supervisor"), "{meta}\n{ev}");
}

/// Under full power, a worker whose every turn is short (`min_work` an hour) and says the same is continued twice, then held under the stalled exception without escalating.
#[test]
fn a_short_turn_loop_saying_the_same_is_continued_twice_then_held_and_never_escalates() {
    const HELD: &str = "kind=stalled when=later held=same-reply Its program answers aterm's acts with the same reply";
    let inst = boot("b");
    let sid = boot_session(&inst);
    let (_keys, _subs, events) = start_looping_worker(&inst, &sid, "done");
    let sec = Duration::from_secs(1);
    submit(&inst, &sid, "go");
    await_match(&inst, &sid, "Deliberating|suite.is.green");
    let sup = Supervisor::start_with(
        &inst,
        &sid,
        TurnEndTiming {
            min_work: 3600 * sec,
            short_backoff: sec / 2,
            short_backoff_max: sec,
            ..TurnEndTiming::default()
        },
        SupervisorConfig::default(),
    );
    // The hold told, or a third continuation typed past it: the chain moves on either way.
    let submits = || {
        worker_events(&events)
            .iter()
            .filter(|(_, kind, _)| kind == "submit")
            .count()
    };
    let told = moving(&inst, || {
        let j = journal(&inst);
        (j.contains(HELD) || submits() > 3).then_some(j)
    })
    .unwrap_or_else(|_| journal(&inst));
    let meta = ctl_ok(&inst, &[&format!("@{sid}"), "meta"]);
    let out = sup.stop();
    let at = stamps_when(&inst, "submit", 3);
    let ev = evidence(&inst, &out);
    assert!(told.contains(HELD), "{ev}");
    assert_eq!(
        at.len(),
        3,
        "`go` and two continuations, then nothing; {ev}"
    );
    assert!(!meta.contains("attention_owner=supervisor"), "{meta}\n{ev}");
}

/// A turn interrupted with Esc is held for `human_grace_s` without escalating, then continued with Cruise's continuation.
#[test]
fn an_esc_interrupt_is_held_then_continued() {
    let inst = boot("i");
    let sid = boot_session(&inst);
    let (_keys, subs, _events) = start_worker(&inst, &sid, "interrupted");
    let sup = Supervisor::start_with(
        &inst,
        &sid,
        TurnEndTiming {
            min_work: Duration::from_millis(500),
            ..TurnEndTiming::default()
        },
        SupervisorConfig {
            human_grace_s: 3,
            ..SupervisorConfig::default()
        },
    );
    submit(&inst, &sid, "go");
    drawn(&inst, "interrupted");
    let submitted = submits_when(&inst, &subs, 2);
    let meta = ctl_ok(&inst, &[&format!("@{sid}"), "meta"]);
    let out = sup.stop();
    let ev = evidence(&inst, &out);
    assert_eq!(
        submitted.iter().take(2).cloned().collect::<Vec<String>>(),
        ["SUBMIT:go".to_string(), cruise_submit()],
        "{ev}"
    );
    assert!(!meta.contains("attention_owner=supervisor"), "{meta}\n{ev}");
}

/// A weekly wall behind a background shell is published, waited out, and continued once.
#[test]
fn a_weekly_wall_behind_a_shell_is_published_waited_out_and_continued_once() {
    let inst = boot("c");
    let sid = boot_session(&inst);
    let (_keys, subs, _events) = start_worker(&inst, &sid, "wall");
    let sup = Supervisor::start(
        &inst,
        &sid,
        TurnEndTiming {
            reset_grace: Duration::from_secs(1),
            reset_jitter: Duration::ZERO,
            ..TurnEndTiming::default()
        },
    );
    submit(&inst, &sid, "go");
    drawn(&inst, "wall");
    let mut status = String::new();
    let published = moving(&inst, || {
        status = ctl_ok(&inst, &[&format!("@{sid}"), "status"]);
        let wall = status.contains("agent=wall:");
        if !wall {
            std::thread::sleep(Duration::from_millis(80));
        }
        wall.then_some(())
    });
    let submitted = submits_when(&inst, &subs, 2);
    let out = sup.stop();
    let ev = evidence(&inst, &out);
    assert!(
        published.is_ok() && status.contains("agent=wall:usage-weekly"),
        "published as the wall: {published:?} {status}\n{ev}"
    );
    assert_eq!(
        submitted,
        ["SUBMIT:go".to_string(), cruise_submit()],
        "{ev}"
    );
    assert!(
        continued_with(&out, "usage-resume@v1", CONTINUE_TEXT),
        "{ev}"
    );
}

/// A weekly wall on a displaced grid (cursor on the row below the prompt) is continued with Cruise's continuation, never a bare Enter.
#[test]
fn a_weekly_wall_on_a_displaced_grid_is_continued_with_keep_going_not_a_bare_enter() {
    let inst = boot("g");
    let sid = boot_session(&inst);
    let (_keys, subs, _events) = start_worker(&inst, &sid, "walldisplaced");
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
    submit(&inst, &sid, "go");
    let submitted = submits_when(&inst, &subs, 2);
    let out = sup.stop();
    let ev = evidence(&inst, &out);
    assert_eq!(
        submitted,
        ["SUBMIT:go".to_string(), cruise_submit()],
        "{ev}"
    );
    assert!(
        continued_with(&out, "usage-resume@v1", CONTINUE_TEXT),
        "{ev}"
    );
}

/// A draft left in the composer at the wall is submitted with a bare Enter after the reset, never overwritten with `keep going`.
#[test]
fn a_draft_left_at_the_wall_is_submitted_after_the_reset_not_keep_going() {
    let inst = boot("j");
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
    submit(&inst, &sid, "go");
    let submitted = submits_when(&inst, &subs, 2);
    let out = sup.stop();
    let ev = evidence(&inst, &out);
    assert_eq!(
        submitted,
        ["SUBMIT:go", "SUBMIT:"],
        "the draft is submitted as it stands; {ev}"
    );
    assert!(
        out.contains("rule=usage-resume@v1") && !out.contains("keep going"),
        "{ev}"
    );
}

/// A weekly wall scrolled out of view is still continued at its reset.
#[test]
fn a_weekly_wall_scrolled_out_of_view_is_still_continued_at_its_reset() {
    let inst = boot("k");
    let sid = boot_session(&inst);
    let (_keys, subs, _events) = start_worker(&inst, &sid, "wallscroll");
    let sup = Supervisor::start(
        &inst,
        &sid,
        TurnEndTiming {
            reset_grace: Duration::from_secs(1),
            reset_jitter: Duration::ZERO,
            ..TurnEndTiming::default()
        },
    );
    submit(&inst, &sid, "go");
    drawn(&inst, "wallscrolled");
    let submitted = submits_when(&inst, &subs, 2);
    let out = sup.stop();
    let ev = evidence(&inst, &out);
    assert_eq!(
        submitted,
        ["SUBMIT:go".to_string(), cruise_submit()],
        "{ev}"
    );
    assert!(
        continued_with(&out, "usage-resume@v1", CONTINUE_TEXT),
        "{ev}"
    );
}

/// A loop that replaces another at a scrolled wall continues waiting for the recorded reset and submits once.
#[test]
fn a_loop_that_replaces_another_at_a_scrolled_wall_goes_on_waiting_for_its_reset() {
    let inst = boot("o");
    let sid = boot_session(&inst);
    let (_keys, subs, _events) = start_worker(&inst, &sid, "wallscroll");
    let timing = |reset_grace: Duration| TurnEndTiming {
        reset_grace,
        reset_jitter: Duration::ZERO,
        ..TurnEndTiming::default()
    };
    let first = Supervisor::start(&inst, &sid, timing(TurnEndTiming::default().reset_grace));
    submit(&inst, &sid, "go");
    let scrolled_at = drawn(&inst, "wallscrolled");
    if let Ok(left) = (scrolled_at + Duration::from_millis(800)).duration_since(SystemTime::now()) {
        std::thread::sleep(left);
    }
    let ledger = std::fs::read_to_string(inst.tmp.join(LEDGER)).unwrap_or_default();
    let said_first = first.stop();
    assert!(
        ledger.contains("wall-wait@v1"),
        "the first loop wrote the wall down:\n{ledger}\n{said_first}"
    );
    let second = Supervisor::start(&inst, &sid, timing(Duration::from_secs(1)));
    let submitted = submits_when(&inst, &subs, 2);
    let said_second = second.stop();
    assert_eq!(
        submitted,
        ["SUBMIT:go".to_string(), cruise_submit()],
        "the successor: {}\nthe first said:\n{said_first}",
        evidence(&inst, &said_second)
    );
    assert!(
        !said_first.contains("rule=usage-resume@v1"),
        "the first loop typed nothing:\n{said_first}"
    );
    assert!(
        continued_with(&said_second, "usage-resume@v1", CONTINUE_TEXT),
        "the successor said:\n{said_second}"
    );
}
