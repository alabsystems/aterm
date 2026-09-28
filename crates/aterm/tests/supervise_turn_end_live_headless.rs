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
//! * THE 529: the supervisor waits the (shortened) backoff, then types
//!   `keep going` through the fenced write (`send if-gen=` on the judged
//!   read, then a fenced Enter once the composer shows it) — once — and not
//!   before the backoff has run.
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
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use aterm_agent::supervise::phase::composer_text;
use aterm_agent::supervise::policy::turn_end::TurnEndTiming;
use aterm_agent::supervise::prompt::fixtures::{
    BOX_BASH_TOUCH, END_529, GOAL_ACTIVE_SUGGESTION, composer, screen,
};
use aterm_agent::supervise::{
    Ctl, Endpoint, Interrupter, Phase, RelayCtl, Session, SuperviseOpts, SupervisorConfig,
    is_placeholder, worker_phase,
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
/// the decision). On the `box` screen a digit answers the box — logged
/// `PRESS:<digit>` — and the turn ends on `done`. The
/// composer is redrawn once per READ of the terminal, as Claude Code renders
/// a burst of input in one frame — in two frames, half the new text and then
/// all of it: `❯ <text>` from the caret row (the terminal wraps it), the rows
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
stty raw -echo
: > "$keys"; : > "$subs"
exec 3<&0
cr=$(printf '\r'); esc=$(printf '\033')
cur=ready; buf=""
# The caret row and the rows under it are read once per SCREEN, so a composer
# frame is written by builtins alone: no spawn can stall between clearing the
# composer and drawing its text, or between the half frame and the whole one,
# for longer than the supervisor's 500 ms settle.
draw() {
  cur="$1"; printf '\033[H\033[2J'; cat "$dir/$1.scr"
  # The box is answered by a key, never composed into: it has no composer.
  [ -e "$dir/$1.row" ] || return 0
  IFS= read -r row < "$dir/$1.row"
  below=$(cat "$dir/$1.below"; printf .); below="${below%.}"
}
compose() {
  shown="$buf"
  if [ "$mangle" = mangle ] && [ ${#buf} -gt 40 ]; then shown="[Pasted text #1]"; fi
  n=$(( (2 + ${#shown} + cols - 1) / cols )); [ "$n" -lt 1 ] && n=1
  printf '\033[%s;1H\033[J\033[%s;1H%s\033[%s;1H❯ %s' \
    "$row" "$((row + n))" "$below" "$row" "$shown"
}
fill() {
  got=$(dd bs=512 count=1 <&3 2>/dev/null)
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
      case "$c" in [0-9]) printf 'PRESS:%s\n' "$c" >> "$subs"; draw busy; sleep 1; draw done ;; esac
      continue
    fi
    if [ "$c" = "$cr" ]; then
      printf 'SUBMIT:%s\n' "$buf" >> "$subs"
      buf=""; typed=0
      n=$(wc -l < "$subs" | tr -d ' ')
      draw busy
      sleep 2
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

/// Write the screens and the script, start the worker as `claude` in the
/// boot session, and wait for its ready screen.
fn start_worker(inst: &Instance, sid: &str, after: &str) -> (PathBuf, PathBuf) {
    start_worker_with(inst, sid, after, COLUMNS, false)
}

/// [`start_worker`], every turn ending on `after`.
fn start_looping_worker(inst: &Instance, sid: &str, after: &str) -> (PathBuf, PathBuf) {
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
) -> (PathBuf, PathBuf) {
    start_worker_in(inst, sid, after, columns, mangle, false)
}

fn start_worker_in(
    inst: &Instance,
    sid: &str,
    after: &str,
    columns: &str,
    mangle: bool,
    looping: bool,
) -> (PathBuf, PathBuf) {
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
    type_line(
        inst,
        sid,
        &format!(
            "/bin/bash -c 'exec -a claude /bin/sh {} {} {} {} {after} {columns} {} {}'",
            script.display(),
            dir.display(),
            keys.display(),
            subs.display(),
            if mangle { "mangle" } else { "-" },
            if looping { "loop" } else { "once" }
        ),
    );
    await_match(inst, sid, "Ready.for.the.next.stage");
    (keys, subs)
}

/// The hosted loop on its own thread, over its own persistent connection,
/// under a policy with `timing`; stopped by [`Supervisor::stop`], which
/// returns what the loop printed.
struct Supervisor {
    stop: Arc<AtomicBool>,
    cut: Option<Interrupter>,
    handle: std::thread::JoinHandle<String>,
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
        let stop = Arc::new(AtomicBool::new(false));
        let mut ctl = RelayCtl::new(Endpoint::Socket(inst.sock.clone()), None);
        ctl.connect().expect("the supervisor's connection");
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
        Self { stop, cut, handle }
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

/// When each of the first `n` lines of `path` appeared, by polling it (the
/// worker's own log: nothing on the socket announces it), or as many as
/// came within `within`.
fn arrivals(path: &Path, n: usize, within: Duration) -> Vec<Instant> {
    let deadline = Instant::now() + within;
    let mut at = Vec::new();
    while at.len() < n && Instant::now() < deadline {
        let now = std::fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .count();
        while at.len() < now.min(n) {
            at.push(Instant::now());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    at
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
    let (keys, subs) = start_worker(&inst, &sid, "goal");
    let sup = Supervisor::start(&inst, &sid, TurnEndTiming::default());
    // A person's line: the turn runs, then ends on the suggestion.
    type_line(&inst, &sid, "go");
    await_match(&inst, &sid, "I.need.your.decision");
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

/// THE 529, live: nothing before the backoff (1 s here), then ONE `keep
/// going` through the fenced write, echoed into the worker's composer and
/// submitted; the decision after it is escalated.
#[test]
fn a_529_end_is_continued_once_after_its_backoff() {
    let Some(inst) = boot("r") else { return };
    let sid = boot_session(&inst);
    let (_keys, subs) = start_worker(&inst, &sid, "e529");
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
    await_match(&inst, &sid, "API.Error:.529");
    let wall_at = Instant::now();
    let submitted = lines_when(&subs, 2, Duration::from_secs(20));
    let continued_after = wall_at.elapsed();
    await_match(&inst, &sid, "I.need.your.decision");
    let out = sup.stop();
    assert_eq!(
        submitted,
        ["SUBMIT:go", "SUBMIT:keep going"],
        "the loop said:\n{out}"
    );
    assert!(
        continued_after >= backoff - Duration::from_millis(100),
        "continued {continued_after:?} after the wall, before its {backoff:?} backoff; the \
         loop said:\n{out}"
    );
    assert!(out.contains("rule=api-retry@v1 keep going"), "{out}");
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
        let (keys, subs) = start_worker_with(&inst, &sid, "done", "64", mangle);
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
        await_match(&inst, &sid, "Fixed.the.parser");
        if !mangle {
            if let Err(reply) = awaited(&inst, &sid, "I.need.your.decision") {
                let out = sup.stop();
                panic!(
                    "the decision never reached the screen ({reply}); submitted {:?}; keys \
                     {:?}; the loop said:\n{out}\njournal:\n{}",
                    std::fs::read_to_string(&subs).unwrap_or_default(),
                    std::fs::read_to_string(&keys).unwrap_or_default(),
                    journal(&inst)
                );
            }
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
        let (_keys, subs) = start_worker(&inst, &sid, "box");
        let mut policy = SupervisorConfig::default();
        policy.set("approve", approve).expect("a level");
        let sup = Supervisor::start_with(&inst, &sid, TurnEndTiming::default(), policy);
        type_line(&inst, &sid, "go");
        await_match(&inst, &sid, "Bash.command");
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
    let (_keys, subs) = start_worker(&inst, &sid, "stop");
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
    await_match(&inst, &sid, "I.need.your.decision");
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
    let (_keys, subs) = start_looping_worker(&inst, &sid, "done");
    let sec = Duration::from_secs(1);
    type_line(&inst, &sid, "go");
    await_match(&inst, &sid, "On.it");
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
    let at = arrivals(&subs, 6, Duration::from_secs(90));
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
    // No continuation came before its back-off: between two, the worker's
    // 2 s turn and at least the wait.
    let gaps: Vec<Duration> = at[1..].windows(2).map(|w| w[1] - w[0]).collect();
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
/// nobody having come back, continued. (A keystroke through a window, the
/// server's `human_ms=`, needs a window this headless instance does not
/// have: the decider's and the loop's tests pin that one.)
#[test]
fn an_esc_interrupt_holds_the_loop_for_the_grace_then_continues() {
    let Some(inst) = boot("i") else { return };
    let sid = boot_session(&inst);
    let (_keys, subs) = start_worker(&inst, &sid, "interrupted");
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
    await_match(&inst, &sid, "What.should.Claude.do.instead");
    let stopped_at = Instant::now();
    let early = lines_when(&subs, 2, grace - Duration::from_millis(700));
    let submitted = lines_when(&subs, 2, Duration::from_secs(20));
    let after = stopped_at.elapsed();
    let meta = ctl_ok(&inst, &[&format!("@{sid}"), "meta"]);
    let out = sup.stop();
    assert_eq!(
        early,
        ["SUBMIT:go"],
        "held for the grace; the loop said:\n{out}"
    );
    assert_eq!(
        submitted[..2],
        ["SUBMIT:go", "SUBMIT:keep going"],
        "the loop said:\n{out}"
    );
    assert!(
        after >= grace - Duration::from_millis(200),
        "{after:?}\n{out}"
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
    let (_keys, subs) = start_worker(&inst, &sid, "done");
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
