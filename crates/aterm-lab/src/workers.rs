// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `workers [tag] [--keep]`: THE proof that an aterm session runs others as
//! WORKERS with the real Claude Code, and no tokens
//! (`docs/DESIGN-sessions-as-subagents-2026-10-15.md` §8, the first bullet;
//! measured in `docs/measured/workers-lab-2026-10-16.md`).
//!
//! A lab of its own (`workers-<pid>` unless a tag is given): the folder trust
//! seeded, and in the scratch settings Bash allowed and `acceptEdits` the
//! permission mode (2.1.301's default, auto mode, sends each Bash call to its
//! classifier, which the stub cannot answer, and blocks it). Claude Code, the
//! MANAGER, runs in the first tab and is given one prompt; the stub's script
//! (`tool` mode, each conversation keyed on its first prompt: the
//! [`crate::stub`] module) makes it
//!
//! 1. start three workers, each with one Bash `aterm ctl @self spawn
//!    agent=claude - <<'EOF' … EOF` (the shim leads the manager's PATH: its
//!    `aterm` is the aterm under test); one task carries quotes, backticks,
//!    `$(…)`, `${…}` and backslashes, another blank and indented lines;
//! 2. run `aterm ctl await task @w1,@w2,@w3 all since=0` (the sids its spawns
//!    printed) as a Claude Code BACKGROUND Bash, and end its turn with text;
//! 3. when Claude Code's `<task-notification>` of that command opens its next
//!    turn, read each worker's `aterm ctl @<w> result` and end with a summary.
//!
//! Each WORKER is the real Claude Code that the server's `spawn agent=claude`
//! typed into a new tab — the lab starts none — on the manager's Claude home
//! (`CLAUDE_CONFIG_DIR` rides its line), whose settings point it at the stub.
//! Its script runs one Bash (a file in the project) and ends with its report
//! and a line `ATERM-TASK-DONE`. Everything runs in the lab's jail. A worker's
//! shell is the login shell the server spawned, which rebuilds its PATH and
//! lists the shim LAST: an `aterm` a worker typed would be the first one on
//! that PATH, not necessarily the aterm under test. The workers here type none.
//!
//! The checks, each printed `ok`/`FAIL` (exit 1 when one fails):
//!
//! * each worker's `task` is `state=done why=marker` with `result_bytes>0`;
//! * each `result` prints its stubbed report BYTE FOR BYTE (the marker line
//!   dropped), and its header's `bytes=` is the report's length;
//! * each worker's record moved from rev 1 to the rev it reads now through
//!   `working` and `done` alone, every rev recorded on the session's own
//!   `timeline` (rev 1 is the spawn's, before any watch could subscribe to the
//!   session), and each move the instance pushed (`subscribe @* events`) is one
//!   of them: no worker was ever `stalled`, and no event was dropped;
//! * each worker's first user row (its transcript) is the task the manager
//!   sent byte for byte, a blank line, and one line of fact naming the manager;
//! * each worker ran its Bash (the file it wrote holds its token);
//! * the manager's three spawns printed `OK <sid>` for the three workers the
//!   instance created; its background await returned (the
//!   `<task-notification>` row, `completed`, its output three `state=done`
//!   rows, read only where Claude Code writes the manager's task output under
//!   the temp root the lab gave it, a new 0700 folder in the run's scratch
//!   (`CLAUDE_CODE_TMPDIR`), through no link and at most 64 KiB of it) and
//!   opened the turn in which it read the three reports and ended with the
//!   summary;
//! * `survey` shows the four Claude Code sessions, each with what it was asked
//!   and its last words, within the budget (24,000 characters, 110 a session
//!   line), and `survey --json` the same records;
//! * the egress witness saw no external endpoint, and saw the four Claude
//!   Codes connect to the stub.
//!
//! With `--upgrade <newer-claude-bin>` on a lab that runs an older build
//! (`LAB_CLAUDE`), the live upgrade of done workers follows: `twin` presents
//! the newer build, the instance moves every session onto it, and each worker
//! must come back `done:resumed` with its record and its result as they were.
//!
//! With `--early` the manager's prompt rides Claude Code's command line, so
//! its first turn and the three spawns begin before the instance has read its
//! screen and named its program: the race three runs met on 2026-10-18, when
//! each worker's line carried plain `claude` and every worker stalled at its
//! startup deadline. Each worker's line must then follow the manager's own
//! Claude Code read off its terminal (`aterm.log`: `launch line typed …
//! read from its terminal`). A failed run of either kind prints each
//! worker's screen and keeps it in the logs (`workers-text.txt`).
//!
//! The timings it prints are read off one machine clock: the manager's `OK
//! <sid>` rows and the workers' own rows (the first prompt, the final reply)
//! in their transcripts, the task moves as the lab read their pushes, and the
//! manager's wake row. NOT OBSERVED: a worker under another identity, a
//! worker that asks a question or meets a wall, a manager on another
//! instance, the folder-trust dialog (seeded here; `selftest` has the
//! harness answer it), auto mode (its classifier asks the API, which the
//! stub cannot answer).

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::LAB;
use crate::door::Door;
use crate::lab::{Launch, UpOpts};
use crate::pyjson::J;
use crate::run::{Run, is_run};
use crate::util::{atomic_write, now, stamp};
use crate::{Fail, Res, stop};

/// The marker a worker ends its report with (what the harness reads as done).
const MARKER: &str = "ATERM-TASK-DONE";

/// One worker of the proof.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Worker {
    /// The token its task carries (the stub keys its script on it).
    pub token: String,
    /// Its task, as the manager's heredoc carries it (no `EOF` line, no trailing newline).
    pub task: String,
    /// The file its Bash writes in the project dir.
    pub file: String,
    /// Its report: the final message before the marker.
    pub report: String,
}

/// What the manager is asked and every conversation's script.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    /// The manager's token.
    pub mgr: String,
    /// The manager's prompt (one line, holding the token).
    pub prompt: String,
    /// The three workers.
    pub workers: Vec<Worker>,
    /// The text the manager ends its first turn with.
    pub waiting: String,
    /// The manager's summary, the last thing it says.
    pub summary: String,
}

impl Plan {
    /// The plan of one run, its tokens made unique by `nonce`.
    #[must_use]
    pub fn new(nonce: &str) -> Plan {
        let mgr = format!("LABMGR-{nonce}");
        let tok = |k: u32| format!("LABW{k}-{nonce}");
        let workers = vec![
            Worker {
                token: tok(1),
                task: format!(
                    "{}: write the file w1.out in this folder with one Bash call, then report what you wrote.",
                    tok(1)
                ),
                file: "w1.out".into(),
                report: "Worker one: wrote w1.out with one Bash call (its token, one line).".into(),
            },
            Worker {
                token: tok(2),
                task: format!(
                    "{}: the bytes of this task must arrive exactly: \"double\" 'single' `backtick` $(echo not-run) ${{HOME}} \\back\\slash !bang & | ; < > * ? ~ # %.\nThen write w2.out.",
                    tok(2)
                ),
                file: "w2.out".into(),
                report: "Worker two: the quotes, `backticks`, $(…), ${…} and \\backslashes came through; wrote w2.out.".into(),
            },
            Worker {
                token: tok(3),
                task: format!(
                    "{}: a task of several lines.\n\n    an indented line, then a blank one\n\nlast line: write w3.out.",
                    tok(3)
                ),
                file: "w3.out".into(),
                report: "Worker three: read all four lines.\n\n- the indented one\n  - and the blank ones\n- wrote w3.out".into(),
            },
        ];
        Plan {
            prompt: format!(
                "{mgr}: start three workers, wait for them in the background, then read each one's result and summarise."
            ),
            mgr,
            workers,
            waiting: "Started three workers; waiting for them in the background.".into(),
            summary: "Summary: all three workers finished and reported (LABSUM).".into(),
        }
    }

    /// The Bash a worker's spawn runs: the task on stdin, byte for byte.
    #[must_use]
    pub fn spawn_command(w: &Worker) -> String {
        format!(
            "aterm ctl @self spawn agent=claude - <<'EOF'\n{}\nEOF",
            w.task
        )
    }

    /// The manager's background wait over the workers its spawns printed.
    #[must_use]
    pub fn await_command(&self) -> String {
        let sel: Vec<String> = (1..=self.workers.len())
            .map(|k| format!("@@SID:{k}@"))
            .collect();
        format!("aterm ctl await task {} all since=0", sel.join(","))
    }

    /// The stub's `tool` script: the manager's steps, then each worker's.
    #[must_use]
    pub fn script(&self) -> J {
        let bash = |when: &str, cmd: String, what: String, bg: bool| {
            let mut input = J::obj([("command", J::s(cmd)), ("description", J::s(what))]);
            if bg {
                input.set("run_in_background", J::Bool(true));
            }
            J::obj([
                ("when", J::s(when)),
                ("name", J::s("Bash")),
                ("input", input),
            ])
        };
        let say = |when: &str, text: &str| J::obj([("when", J::s(when)), ("text", J::s(text))]);
        let mut steps = Vec::new();
        for (k, w) in self.workers.iter().enumerate() {
            steps.push(bash(
                &self.mgr,
                Self::spawn_command(w),
                format!("Start worker {}", k + 1),
                false,
            ));
        }
        steps.push(bash(
            &self.mgr,
            self.await_command(),
            "Wait for the workers".into(),
            true,
        ));
        steps.push(say(&self.mgr, &self.waiting));
        for k in 1..=self.workers.len() {
            steps.push(bash(
                &self.mgr,
                format!("aterm ctl @@SID:{k}@ result"),
                format!("Read worker {k}'s result"),
                false,
            ));
        }
        steps.push(say(&self.mgr, &self.summary));
        for w in &self.workers {
            steps.push(bash(
                &w.token,
                format!(
                    "printf '%s\\n' '{}' > {} && wc -c {}",
                    w.token, w.file, w.file
                ),
                format!("Write {}", w.file),
                false,
            ));
            steps.push(say(&w.token, &format!("{}\n{MARKER}", w.report)));
        }
        J::Arr(steps)
    }
}

/// One row of a Claude Code transcript, as the proof reads it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Row {
    /// `user`, `assistant`, `system`, `attachment`, …
    pub kind: String,
    /// Its stamp (unix ms).
    pub ts: Option<i64>,
    /// Its message's text: the string, or its text blocks joined by newlines.
    pub text: String,
    /// A tool use: its name and input.
    pub tool_use: Option<(String, J)>,
    /// A tool result's text.
    pub result: Option<String>,
    /// The tool use's id, or the one a tool result answers.
    pub tool_id: String,
}

/// An ISO-8601 UTC stamp (`2026-10-16T10:24:42.738Z`) as unix milliseconds.
#[must_use]
pub fn iso_ms(s: &str) -> Option<i64> {
    let (date, time) = s.strip_suffix('Z')?.split_once('T')?;
    let mut d = date.split('-').map(|p| p.parse::<i64>().ok());
    let (y, m, day) = (d.next()??, d.next()??, d.next()??);
    let (hms, frac) = time.split_once('.').unwrap_or((time, "0"));
    let mut t = hms.split(':').map(|p| p.parse::<i64>().ok());
    let (h, mi, sec) = (t.next()??, t.next()??, t.next()??);
    let ms: i64 = format!("{frac:0<3}").get(..3)?.parse().ok()?;
    // days from the civil date (Howard Hinnant's algorithm)
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some((((days * 24 + h) * 60 + mi) * 60 + sec) * 1000 + ms)
}

/// The rows of a transcript's text (a line that does not parse is skipped).
/// Only a `user` or `assistant` row is the conversation's: another kind that
/// carries a message (2.1.301's `api-request-blob`, a copy of what a request
/// sent) keeps its text but gives no tool use or result.
#[must_use]
pub fn rows(text: &str) -> Vec<Row> {
    let blocks_text = |c: &J| crate::stub::text_of(Some(c));
    text.lines()
        .filter_map(|l| {
            let j = J::parse(l).ok()?;
            let kind = j.get("type").and_then(J::as_str).unwrap_or("").to_string();
            let ts = j.get("timestamp").and_then(J::as_str).and_then(iso_ms);
            let content = j.get("message").and_then(|m| m.get("content"));
            let mut row = Row {
                kind,
                ts,
                text: content.map(blocks_text).unwrap_or_default(),
                ..Row::default()
            };
            let said = row.kind == "user" || row.kind == "assistant";
            for b in content
                .and_then(J::as_arr)
                .filter(|_| said)
                .unwrap_or_default()
            {
                let id = |k: &str| b.get(k).and_then(J::as_str).unwrap_or("").to_string();
                match b.get("type").and_then(J::as_str) {
                    Some("tool_use") => {
                        let name = b.get("name").and_then(J::as_str).unwrap_or("").to_string();
                        let input = b.get("input").cloned().unwrap_or(J::Null);
                        row.tool_use = Some((name, input));
                        row.tool_id = id("id");
                    }
                    Some("tool_result") => {
                        row.tool_id = id("tool_use_id");
                        row.result = Some(match b.get("content") {
                            Some(J::Str(s)) => s.clone(),
                            other => crate::stub::text_of(other),
                        });
                    }
                    _ => {}
                }
            }
            Some(row)
        })
        .collect()
}

/// The first user row with words and no tool result: the prompt that opened a conversation.
#[must_use]
pub fn first_prompt(rows: &[Row]) -> Option<&Row> {
    rows.iter()
        .find(|r| r.kind == "user" && r.result.is_none() && !r.text.is_empty())
}

/// The value of `key=` in a reply line (`OK task rev=2 state=done …`).
#[must_use]
pub fn kv<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    line.split_whitespace()
        .find_map(|w| w.strip_prefix(key)?.strip_prefix('='))
}

/// What the instance pushed (`subscribe @* events,sessions`), read off its lines.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Pushed {
    /// `session-created <sid>`, with the wall time it was read.
    pub created: Vec<(String, f64)>,
    /// `task …` moves: the session, the wall time, the payload (`rev= state= …`).
    pub tasks: Vec<(String, f64, String)>,
    /// `GAP` lines: events the timeline dropped before this watch saw them.
    pub gaps: Vec<String>,
}

/// [`Pushed`] of the stream's lines `(wall, line)`; a `sub <local> <sid>` ack names a local id's session.
#[must_use]
pub fn pushed(lines: &[(f64, String)]) -> Pushed {
    let mut sid_of: Vec<(String, String)> = Vec::new();
    let mut p = Pushed::default();
    for (wall, line) in lines {
        let w: Vec<&str> = line.split_whitespace().collect();
        match w.as_slice() {
            ["sub", local, sid, ..] => sid_of.push(((*local).to_string(), (*sid).to_string())),
            ["EVENT", "*", "session-created", sid, ..] => {
                p.created.push(((*sid).to_string(), *wall))
            }
            ["EVENT", local, "task", ..] => {
                let sid = sid_of
                    .iter()
                    .rev()
                    .find(|(l, _)| l == local)
                    .map_or_else(|| format!("local:{local}"), |(_, s)| s.clone());
                let payload = line
                    .split_once(" task ")
                    .map_or("", |(_, rest)| rest)
                    .to_string();
                p.tasks.push((sid, *wall, payload));
            }
            ["GAP", ..] => p.gaps.push(line.clone()),
            _ => {}
        }
    }
    p
}

/// The task record's moves a session's `timeline` recorded, oldest first:
/// each `task-change` row's rev and state.
#[must_use]
pub fn task_moves(timeline: &str) -> Vec<(u64, String)> {
    timeline
        .lines()
        .filter(|l| l.starts_with("event ") && kv(l, "kind") == Some("task-change"))
        .filter_map(|l| {
            let rev = kv(l, "rev")?.parse().ok()?;
            Some((rev, kv(l, "state").unwrap_or("?").to_string()))
        })
        .collect()
}

/// Whether `moves` are every move of a record that now reads `now_rev`, rev 1
/// to it with none missing, and each one `working` or `done`. A move the
/// timeline did not keep is a gap, and fails: what it moved to is unknown.
#[must_use]
pub fn never_stalled(moves: &[(u64, String)], now_rev: Option<u64>) -> bool {
    let Some(now_rev) = now_rev else {
        return false;
    };
    moves.iter().map(|(r, _)| *r).eq(1..=now_rev)
        && moves.iter().all(|(_, s)| s == "working" || s == "done")
}

/// One session's block of a `survey` text: its session line and the excerpt
/// lines indented under it.
#[must_use]
pub fn survey_block(text: &str, sid: &str) -> String {
    let mut lines = text.lines().skip_while(|l| !l.starts_with(sid));
    let mut b = String::new();
    if let Some(head) = lines.next() {
        b.push_str(head);
        b.push('\n');
    }
    for l in lines.take_while(|l| l.starts_with("    ")) {
        b.push_str(l);
        b.push('\n');
    }
    b
}

/// What a session was asked and last said, as [`survey_shown`] looks for it:
/// its sid, a word of what it was asked, and the head and the end of what it
/// last said.
pub type AskedSaid = (String, String, String, String);

/// How many of `asked_said` a `survey` text shows as `claude` with both
/// excerpts: its `asked:` holding the word, its `said:` the head of what it
/// said or its end — a finished turn's `said:` cut by the read keeps its end
/// (`…` first), where its report puts its result.
#[must_use]
pub fn survey_shown(text: &str, asked_said: &[AskedSaid]) -> usize {
    asked_said
        .iter()
        .filter(|(sid, asked, head, end)| {
            let b = survey_block(text, sid);
            b.lines()
                .next()
                .is_some_and(|l| l.split_whitespace().nth(1) == Some("claude"))
                && b.lines()
                    .any(|l| l.trim_start().starts_with("asked:") && l.contains(asked.as_str()))
                && b.lines().any(|l| {
                    l.trim_start().starts_with("said:")
                        && (l.contains(head.as_str()) || l.contains(end.as_str()))
                })
        })
        .count()
}

/// The instance's pushes, read on a thread of their own from one control connection.
struct Watch {
    lines: Arc<Mutex<Vec<(f64, String)>>>,
    stream: Option<aterm_uds::CtlStream>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Watch {
    /// `subscribe @* events,sessions` on the lab behind `door`.
    fn start(door: &Door<'_>) -> Result<Watch, String> {
        let mut client = door.connect()?;
        let stream = client.transport().try_clone().map_err(|e| e.to_string())?;
        let reply = client
            .request_line("subscribe @* events,sessions")
            .map_err(|e| format!("subscribe: {e}"))?;
        if !reply.starts_with("OK") {
            return Err(format!("subscribe: {reply}"));
        }
        let lines: Arc<Mutex<Vec<(f64, String)>>> = Arc::default();
        let held = Arc::clone(&lines);
        let thread = std::thread::spawn(move || {
            while let Ok(line) = client.next_pushed() {
                held.lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push((now(), line));
            }
        });
        Ok(Watch {
            lines,
            stream: Some(stream),
            thread: Some(thread),
        })
    }

    /// What was pushed so far.
    fn pushed(&self) -> Pushed {
        pushed(
            &self
                .lines
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    }

    /// End the subscription.
    fn stop(&mut self) {
        if let Some(s) = self.stream.take() {
            let _ = s.shutdown(std::net::Shutdown::Both);
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        self.stop();
    }
}

/// The checks, as they are printed.
#[derive(Default)]
struct Checks {
    n: u32,
    bad: u32,
}

impl Checks {
    fn check(&mut self, ok: bool, label: &str) -> bool {
        self.n += 1;
        if ok {
            println!("  ok   {label}");
        } else {
            self.bad += 1;
            println!("  FAIL {label}");
        }
        ok
    }
}

/// The file of the run (outside the jail's scratch, written by the lab) a failed run keeps each
/// worker tab's screen in; `down` keeps it with the logs.
pub const WORKER_TEXTS: &str = "workers-text.txt";

/// A FAILED RUN KEEPS WHAT EACH WORKER'S TAB SHOWS: its `status` line and its `text`, printed and
/// in the run's [`WORKER_TEXTS`]. A worker that never started shows there what its shell made of
/// its launch line, or that none was typed (2026-10-18: three such stalls were kept with nothing
/// but the startup deadline's line).
fn keep_worker_texts(run: &Run, door: &Door, sids: &[String]) {
    let mut all = String::new();
    for sid in sids {
        let at = format!("@{sid}");
        let status = door.out(&[at.as_str(), "status"]);
        let text = door.out(&[at.as_str(), "text"]);
        all.push_str(&format!("==== {sid}\n{}\n{text}\n", status.trim_end()));
        println!("  screen    {sid}");
        for l in text.lines().filter(|l| !l.trim().is_empty()) {
            println!("    | {l}");
        }
    }
    if let Err(e) = atomic_write(&run.run.join(WORKER_TEXTS), all.as_bytes()) {
        println!("  (the workers' screens were not kept: {e})");
    }
}

/// Every transcript under the scratch Claude home (`projects/*/*.jsonl`), each read through no link.
fn transcripts(run: &Run) -> Vec<(PathBuf, Vec<Row>)> {
    let root = run.w.join("home/.claude/projects");
    let mut out = Vec::new();
    for dir in fs::read_dir(&root).into_iter().flatten().flatten() {
        for f in fs::read_dir(dir.path()).into_iter().flatten().flatten() {
            let p = f.path();
            if p.extension().is_some_and(|e| e == "jsonl")
                && let Ok(Some(b)) = crate::sys::scratch_read(&p)
            {
                out.push((p, rows(&String::from_utf8_lossy(&b))));
            }
        }
    }
    out
}

/// The transcript whose first prompt holds `token`, with its path.
fn transcript_at<'a>(
    all: &'a [(PathBuf, Vec<Row>)],
    token: &str,
) -> Option<&'a (PathBuf, Vec<Row>)> {
    all.iter()
        .find(|(_, r)| first_prompt(r).is_some_and(|p| p.text.contains(token)))
}

/// The rows of the transcript whose first prompt holds `token`.
fn transcript_of<'a>(all: &'a [(PathBuf, Vec<Row>)], token: &str) -> Option<&'a [Row]> {
    transcript_at(all, token).map(|(_, r)| r.as_slice())
}

/// Whether the manager's transcript holds its summary: the last row its
/// scripted turns write, which may land a moment after the screen draws it.
fn summary_written(run: &Run, plan: &Plan) -> bool {
    transcript_of(&transcripts(run), &plan.mgr).is_some_and(|rows| {
        rows.iter()
            .any(|r| r.kind == "assistant" && r.text == plan.summary)
    })
}

/// Seconds between two unix-ms stamps, for the report.
fn secs(from: i64, to: i64) -> String {
    #[allow(clippy::cast_precision_loss)]
    let s = (to - from) as f64 / 1000.0;
    format!("{s:.2} s")
}

/// A wall time (seconds since the epoch) as unix ms.
fn wall_ms(w: f64) -> i64 {
    #[allow(clippy::cast_possible_truncation)]
    let ms = (w * 1000.0).round() as i64;
    ms
}

/// The sids of the manager's spawn results, in order (`OK <sid>`), each tool result once.
fn spawned(mgr: &[Row]) -> Vec<String> {
    let mut seen: Vec<&str> = Vec::new();
    let mut out = Vec::new();
    for r in mgr {
        let Some(t) = r.result.as_deref() else {
            continue;
        };
        if !r.tool_id.is_empty() {
            if seen.contains(&r.tool_id.as_str()) {
                continue;
            }
            seen.push(r.tool_id.as_str());
        }
        if let Some(h) = t
            .trim()
            .strip_prefix("OK s-")
            .filter(|h| h.bytes().all(|b| b.is_ascii_hexdigit()))
        {
            out.push(format!("s-{h}"));
        }
    }
    out
}

/// What the run concluded.
enum Verdict {
    Pass,
    Failed,
}

/// The text a task notification holds between `<name>` and `</name>`
/// (`task-id`, `output-file`, `status`).
fn notification_field<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    let (_, rest) = text.split_once(&format!("<{name}>"))?;
    rest.split_once(&format!("</{name}>"))
        .map(|(v, _)| v.trim())
}

/// Where Claude Code, run with `CLAUDE_CODE_TMPDIR=<tmp_root>`, writes the
/// output of the background command `task_id` that the conversation of
/// `transcript` (`<home>/projects/<project>/<session>.jsonl`) started:
/// `<tmp_root>/claude-<uid>/<project>/<session>/tasks/<task_id>.output`. The
/// lab gives the manager a `tmp_root` of its own in the run's scratch
/// ([`run_workers`]) and reads the await's output there and nowhere else: a
/// notification's `<output-file>` is text the jailed manager wrote, so it
/// must name this path, which the lab builds from its own facts. `None` for a
/// task id that is not one word of letters, digits, `_` and `-`, and for a
/// project or session that is no folder name of its own (empty, `.`, `..`).
fn task_output_path(tmp_root: &Path, transcript: &Path, task_id: &str) -> Option<PathBuf> {
    let word = !task_id.is_empty()
        && task_id.len() <= 64
        && task_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    let named = |part: &str| !matches!(part, "" | "." | "..") && !part.contains('/');
    if !word {
        return None;
    }
    let session = transcript.file_stem()?.to_str().filter(|s| named(s))?;
    let project = transcript
        .parent()?
        .file_name()?
        .to_str()
        .filter(|p| named(p))?;
    Some(
        tmp_root
            .join(format!("claude-{}", crate::sys::getuid()))
            .join(project)
            .join(session)
            .join("tasks")
            .join(format!("{task_id}.output")),
    )
}

/// The most of the await's output the lab reads: its three rows and the exit
/// line Claude Code adds are well under one KiB.
const AWAIT_OUTPUT_MAX: u64 = 64 * 1024;

/// The text at `path` through the scratch reader (no link anywhere on its
/// path, the lab user's regular file with one name), read only while it is
/// at most `max` bytes; why it was not read otherwise.
fn read_bounded(path: &Path, max: u64) -> Result<String, String> {
    let file = crate::sys::scratch_file(path)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("nothing at {}", path.display()))?;
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut std::io::Read::take(file, max + 1), &mut bytes)
        .map_err(|e| format!("reading {}: {e}", path.display()))?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > max {
        return Err(format!("{} holds more than {max} bytes", path.display()));
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Wait, in slices that an interrupt ends, until `ready` holds or `limit` passes; whether it held.
fn wait_for(limit: Duration, mut ready: impl FnMut() -> bool) -> Result<bool, stop::Stopped> {
    let end = std::time::Instant::now() + limit;
    loop {
        if ready() {
            return Ok(true);
        }
        if std::time::Instant::now() >= end {
            return Ok(false);
        }
        stop::sleep(Duration::from_millis(200))?;
    }
}

/// The `upgrade=` column of session `sid` in an `ls` reply (empty when it has none).
#[must_use]
pub fn upgrade_of(ls: &str, sid: &str) -> String {
    ls.lines()
        .find(|l| l.split_whitespace().nth(2) == Some(sid))
        .and_then(|l| kv(l, "upgrade"))
        .unwrap_or("")
        .to_string()
}

/// What a worker's record and result say, for the drill's before and after:
/// the record's `rev= state= why= result_bytes=`, and the result as printed.
fn held(door: &Door<'_>, sid: &str) -> Result<(String, String), Fail> {
    let at = format!("@{sid}");
    let task = door.out(&[at.as_str(), "task"]);
    let rec = ["rev", "state", "why", "result_bytes"]
        .iter()
        .map(|k| format!("{k}={}", kv(&task, k).unwrap_or("-")))
        .collect::<Vec<_>>()
        .join(" ");
    Ok((rec, door.ctl(&[at.as_str(), "result"])?.stdout))
}

/// THE LIVE UPGRADE OF DONE WORKERS (`--upgrade <newer-claude-bin>`, on a lab
/// that runs an older build: `LAB_CLAUDE`): `twin` presents `bin` as a newer
/// managed Claude Code, the instance's live upgrade moves every session onto
/// it, and each done worker must come back with its record and its result as
/// they were (`docs/DESIGN-sessions-as-subagents-2026-10-15.md` §2.3: the
/// result is a snapshot, never read again).
fn upgrade_drill(
    run: &Run,
    door: &Door<'_>,
    sessions: &[String],
    workers: &[(String, String)],
    bin: &str,
    c: &mut Checks,
) -> Result<(), Fail> {
    let mut before = Vec::new();
    for (sid, _) in workers {
        before.push(held(door, sid)?);
    }
    let t0 = now();
    crate::verbs::twin(run, &[bin.to_string()])?;
    let mut moved: Vec<(String, f64, String)> = Vec::new();
    let ended = wait_for(Duration::from_secs(240), || {
        let ls = door.out(&["ls"]);
        for sid in sessions {
            let u = upgrade_of(&ls, sid);
            let over = ["done/", "stalled/", "skipped/"]
                .iter()
                .any(|p| u.starts_with(p));
            if over && !moved.iter().any(|(s, _, _)| s == sid) {
                moved.push((sid.clone(), now() - t0, u));
            }
        }
        moved.len() == sessions.len()
    })?;
    for (sid, t, u) in &moved {
        println!("  upgrade   {sid} {u} {t:.1} s after the twin");
    }
    let done = moved
        .iter()
        .filter(|(_, _, u)| u.starts_with("done/"))
        .count();
    c.check(
        ended && done == sessions.len(),
        &format!(
            "upgrade: the live upgrade moved all {} sessions onto the newer build ({done} done within 240 s)",
            sessions.len()
        ),
    );
    let ledger = crate::util::scratch_text(
        &run.state()
            .join("harness/claude-harness/upgrade/ledger.jsonl"),
    );
    for ((sid, report), (rec, res)) in workers.iter().zip(&before) {
        let (rec_after, res_after) = held(door, sid)?;
        let resumed = ledger.lines().filter_map(|l| J::parse(l).ok()).any(|r| {
            r.get("tab").and_then(J::as_str) == Some(sid.as_str())
                && r.get("step").and_then(J::as_str) == Some("done:resumed")
        });
        c.check(
            resumed && rec_after == *rec && res_after == *res && res_after == format!("{report}\n"),
            &format!(
                "upgrade: {sid} resumed on the newer build (the ledger's done:resumed: {resumed}) with its record ({rec_after}) and its result byte for byte as before"
            ),
        );
    }
    Ok(())
}

/// One proof on the lab `tag`, which [`cmd_workers`] brought up; with
/// `upgrade`, the live-upgrade drill after it ([`upgrade_drill`]).
#[allow(clippy::too_many_lines)]
fn run_workers(tag: &str, upgrade: Option<&str>, early: bool) -> Result<Verdict, Fail> {
    let could_not = |m: &str| Fail::exit(3, format!("workers: COULD NOT RUN: {m}"));
    let run = Run::load(tag)?;
    let door = Door::new(&run)?;
    let mut b = [0u8; 4];
    let _ = aterm_uds::rand::fill(&mut b);
    let plan = Plan::new(&format!("{:08x}", u32::from_ne_bytes(b)));
    println!(
        "workers: lab '{tag}', Claude Code {}, {}",
        run.claude_ver,
        run.need_aterm()?.display()
    );

    // ---- the scratch: Bash allowed, accept-edits mode, the script, the stub in `tool` mode
    crate::verbs::settings_merge(
        &run,
        &J::obj([(
            "permissions",
            J::obj([
                ("allow", J::Arr(vec![J::s("Bash")])),
                ("defaultMode", J::s("acceptEdits")),
            ]),
        )]),
    )?;
    atomic_write(&run.w.join("tool.json"), plan.script().py().as_bytes())
        .map_err(|e| could_not(&format!("cannot write the script: {e}")))?;
    crate::stub::control(&run.port, "/__mode?m=tool")
        .map_err(|e| could_not(&format!("the stub did not take mode tool: {e}")))?;
    let mut watch = Watch::start(&door).map_err(|e| could_not(&e))?;

    // ---- the manager, and its one prompt. Its temp root is a new 0700 folder in the scratch, so the
    // output of its background await lands inside the run's own files, where the lab reads it
    // (`task_output_path`). The workers keep Claude Code's default, /private/tmp/claude-<uid>: aterm
    // strips `CLAUDE*` from the shells it spawns, and nothing of theirs is read there
    let tmp_root = PathBuf::from(
        crate::sys::make_temp_dir(&format!("{}/tmp.XXXXXX", run.w.display()))
            .map_err(|e| could_not(&format!("cannot make the manager's temp dir: {e}")))?,
    );
    // `--early`: the prompt rides Claude Code's argv, so its first turn (and the three spawns) can
    // begin before the instance has read the manager's screen and named its program (the race
    // three runs met on 2026-10-18): what a worker's line follows then is the starter's terminal
    let t_launch = now();
    crate::lab::launch_claude(
        &run,
        &Launch {
            envs: vec![format!("CLAUDE_CODE_TMPDIR={}", tmp_root.display())],
            no_wait: true,
            args: if early {
                vec![plan.prompt.clone()]
            } else {
                Vec::new()
            },
            ..Launch::default()
        },
    )
    .map_err(|f| {
        if f.stopped.is_some() {
            f
        } else {
            could_not(&format!(
                "Claude Code could not be launched: {}",
                f.msg.unwrap_or_default()
            ))
        }
    })?;
    let t_prompt = if early {
        t_launch
    } else {
        if !door.seen("❯", 60)? {
            return Err(could_not(
                "the manager's Claude Code never drew its composer",
            ));
        }
        let t_prompt = now();
        if !crate::verbs::turn(&door, &plan.prompt)? {
            return Err(could_not("the manager's composer did not take the prompt"));
        }
        t_prompt
    };
    let mgr_sid = crate::verbs::sid(&door);
    println!(
        "  manager   {mgr_sid}: prompt {} at {}",
        if early {
            "on its command line, launched"
        } else {
            "sent"
        },
        stamp(t_prompt)
    );

    // ---- three workers done (or one stalled or gone), then the manager's summary on its screen
    let n = plan.workers.len();
    let settled = |p: &Pushed| {
        let workers: Vec<&String> = p
            .created
            .iter()
            .map(|(s, _)| s)
            .filter(|s| **s != mgr_sid)
            .collect();
        workers.len() >= n
            && workers.iter().all(|s| {
                p.tasks
                    .iter()
                    .rev()
                    .find(|(t, _, _)| t == *s)
                    .is_some_and(|(_, _, pl)| {
                        matches!(kv(pl, "state"), Some("done" | "stalled" | "exited"))
                    })
            })
    };
    let all_settled = wait_for(Duration::from_secs(240), || settled(&watch.pushed()))?;
    let summary_seen = all_settled && door.seen("LABSUM", 120)?;
    let t_end = now();
    // the screen draws the summary a moment before the transcript holds it: wait for the row itself
    // (a one-minute hang detector; it must land, and the check below says so if it did not)
    if summary_seen {
        wait_for(Duration::from_secs(60), || summary_written(&run, &plan))?;
    }
    watch.stop();
    let p = watch.pushed();
    println!(
        "  settled   workers {} within 240 s; summary {} ({:.1} s after the prompt)",
        if all_settled {
            "settled"
        } else {
            "NOT settled"
        },
        if summary_seen { "drawn" } else { "NOT drawn" },
        t_end - t_prompt
    );

    // ---- the evidence
    let all = transcripts(&run);
    let mgr_at = transcript_at(&all, &plan.mgr);
    let mgr = mgr_at.map(|(_, r)| r.as_slice()).unwrap_or_default();
    let sids = spawned(mgr);
    let created: Vec<(String, f64)> = p
        .created
        .iter()
        .filter(|(s, _)| *s != mgr_sid)
        .cloned()
        .collect();
    let mut c = Checks::default();
    c.check(
        summary_seen,
        "the manager drew its summary: its scripted turns ran end to end",
    );
    c.check(
        sids.len() == n
            && created.len() == n
            && sids.iter().all(|s| created.iter().any(|(c, _)| c == s)),
        &format!(
            "the manager's {n} spawns printed OK <sid> for the {n} sessions the instance created (spawned {sids:?}, created {:?})",
            created.iter().map(|(s, _)| s).collect::<Vec<_>>()
        ),
    );
    c.check(
        p.gaps.is_empty(),
        &format!(
            "the event watch dropped nothing (no GAP){}",
            if p.gaps.is_empty() {
                String::new()
            } else {
                format!(": {:?}", p.gaps)
            }
        ),
    );

    let proj = fs::canonicalize(&run.proj).unwrap_or_else(|_| PathBuf::from(&run.proj));
    let mut done_at: Vec<f64> = Vec::new();
    for (k, w) in plan.workers.iter().enumerate() {
        let label = format!("w{}", k + 1);
        let Some(sid) = sids.get(k) else {
            c.check(
                false,
                &format!("{label}: the manager's spawn printed no sid"),
            );
            continue;
        };
        let at = format!("@{sid}");
        println!("  {label}        {sid}");
        // the task record
        let task = door.out(&[at.as_str(), "task"]);
        let rb = kv(&task, "result_bytes")
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0);
        c.check(
            kv(&task, "state") == Some("done") && kv(&task, "why") == Some("marker") && rb > 0,
            &format!(
                "{label}: task state=done why=marker result_bytes>0 ({})",
                task.trim().chars().take(140).collect::<String>()
            ),
        );
        // never stalled: every move of its record, rev 1 (the spawn's, before any watch could
        // subscribe to the session) to the rev it reads now, from the session's own timeline; and
        // every move the watch was pushed is one of them
        let moves: Vec<&(String, f64, String)> =
            p.tasks.iter().filter(|(s, _, _)| s == sid).collect();
        let rev_state = |pl: &str| {
            format!(
                "{}:{}",
                kv(pl, "rev").unwrap_or("?"),
                kv(pl, "state").unwrap_or("?")
            )
        };
        let timeline = door.out(&[at.as_str(), "timeline"]);
        let recorded = task_moves(&timeline);
        let pushed_states: Vec<String> = moves.iter().map(|(_, _, pl)| rev_state(pl)).collect();
        let now_rev = kv(&task, "rev").and_then(|v| v.parse::<u64>().ok());
        c.check(
            never_stalled(&recorded, now_rev)
                && !moves.is_empty()
                && pushed_states
                    .iter()
                    .all(|m| recorded.iter().any(|(r, s)| *m == format!("{r}:{s}"))),
            &format!(
                "{label}: never stalled: its record moved rev 1 to {} through working and done alone, every rev recorded ({:?}), and each move pushed is one of them ({pushed_states:?})",
                now_rev.map_or_else(|| "?".to_string(), |r| r.to_string()),
                recorded
                    .iter()
                    .map(|(r, s)| format!("{r}:{s}"))
                    .collect::<Vec<_>>()
            ),
        );
        if let Some((_, t, _)) = moves
            .iter()
            .find(|(_, _, pl)| kv(pl, "state") == Some("done"))
        {
            done_at.push(*t);
        }
        // the result, byte for byte
        let res = door.ctl(&[at.as_str(), "result"])?;
        let header = res.stderr.lines().find(|l| l.contains("OK ")).unwrap_or("");
        let want = format!("{}\n", w.report);
        c.check(
            res.code == 0 && res.stdout == want,
            &format!(
                "{label}: result is its stubbed report byte for byte ({} bytes){}",
                w.report.len(),
                if res.stdout == want {
                    String::new()
                } else {
                    format!(": got {:?}", res.stdout)
                }
            ),
        );
        c.check(
            kv(header, "bytes") == Some(w.report.len().to_string().as_str()),
            &format!(
                "{label}: result header says bytes={} ({header})",
                w.report.len()
            ),
        );
        // the first user row: the task as sent, a blank line, one line of fact naming the manager
        let wt = transcript_of(&all, &w.token).unwrap_or_default();
        let first = first_prompt(wt).map(|r| r.text.clone()).unwrap_or_default();
        let fact = first.strip_prefix(&format!("{}\n\n", w.task));
        c.check(
            fact.is_some_and(|f| !f.contains('\n') && f.contains(&format!("`{mgr_sid}`"))),
            &format!(
                "{label}: its first user row is the task the manager sent byte for byte, then one line of fact naming {mgr_sid}{}",
                if fact.is_some() {
                    String::new()
                } else {
                    format!(": got {:?}", first.chars().take(300).collect::<String>())
                }
            ),
        );
        // its Bash ran, in the project
        // through no link: the project's physical path (`/tmp` is one), which the jail may not move
        let wrote = crate::util::scratch_text(&proj.join(&w.file));
        c.check(
            wrote.trim() == w.token,
            &format!("{label}: its Bash ran: {} holds its token", w.file),
        );
        // timings: its spawn's `OK <sid>` row in the manager's transcript, then its own rows, then the push
        let spawn_ms = mgr
            .iter()
            .find(|r| {
                r.kind == "user"
                    && r.result
                        .as_deref()
                        .is_some_and(|t| t.trim() == format!("OK {sid}"))
            })
            .and_then(|r| r.ts);
        let prompt_ms = first_prompt(wt).and_then(|r| r.ts);
        let reply_ms = wt
            .iter()
            .rev()
            .find(|r| r.kind == "assistant" && r.text.contains(MARKER))
            .and_then(|r| r.ts);
        let done_ms = moves
            .iter()
            .find(|(_, _, pl)| kv(pl, "state") == Some("done"))
            .map(|(_, t, _)| wall_ms(*t));
        if let (Some(sp), Some(pr), Some(re), Some(dn)) = (spawn_ms, prompt_ms, reply_ms, done_ms) {
            println!(
                "  {label} time  OK <sid> -> first prompt {}; first prompt -> final reply {}; final reply -> done pushed {}",
                secs(sp, pr),
                secs(pr, re),
                secs(re, dn)
            );
        }
    }

    // ---- the manager: the background await returned and opened the turn that read the results
    let bg = mgr.iter().position(|r| {
        r.tool_use.as_ref().is_some_and(|(name, input)| {
            name == "Bash"
                && input.get("run_in_background").is_some_and(J::truthy)
                && input
                    .get("command")
                    .and_then(J::as_str)
                    .is_some_and(|cmd| cmd.contains("await task"))
        })
    });
    let wake = bg.and_then(|i| {
        mgr.iter()
            .enumerate()
            .skip(i + 1)
            .find(|(_, r)| r.kind == "user" && r.text.contains("<task-notification>"))
    });
    let woke = wake.is_some_and(|(_, r)| r.text.contains("completed"));
    c.check(
        bg.is_some() && woke,
        &format!(
            "the manager's await ran as a background Bash, and its <task-notification> (completed) opened a turn{}",
            wake.map_or_else(String::new, |(_, r)| format!(
                ": {}",
                r.text.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(160).collect::<String>()
            ))
        ),
    );
    let wake_text = wake.map_or("", |(_, r)| r.text.as_str());
    let named = notification_field(wake_text, "output-file");
    let expected = mgr_at.and_then(|(path, _)| {
        notification_field(wake_text, "task-id")
            .and_then(|id| task_output_path(&tmp_root, path, id))
    });
    let (out, unread) = match (named, &expected) {
        (Some(n), Some(e)) if Path::new(n) == e.as_path() => {
            match read_bounded(e, AWAIT_OUTPUT_MAX) {
                Ok(text) => (text, String::new()),
                Err(why) => (String::new(), format!("; not read: {why}")),
            }
        }
        _ => (
            String::new(),
            format!(
                "; not read: the notification's <output-file> {named:?} is not {expected:?}, where Claude Code writes this conversation's task output"
            ),
        ),
    };
    let done_rows = out
        .lines()
        .filter(|l| {
            sids.iter().any(|s| l.starts_with(&format!("@{s} "))) && kv(l, "state") == Some("done")
        })
        .count();
    c.check(
        done_rows == n,
        &format!(
            "the await's output has {n} rows state=done, one per worker ({done_rows}: {:?}){unread}",
            out.trim().chars().take(400).collect::<String>()
        ),
    );
    let after: Vec<&Row> = wake.map_or_else(Vec::new, |(i, _)| mgr[i + 1..].iter().collect());
    let read_back = plan
        .workers
        .iter()
        .filter(|w| {
            after
                .iter()
                .any(|r| r.result.as_deref().is_some_and(|t| t.contains(&w.report)))
        })
        .count();
    let summary_said = after
        .iter()
        .rev()
        .find(|r| r.kind == "assistant" && !r.text.is_empty())
        .is_some_and(|r| r.text == plan.summary);
    c.check(
        read_back == n && summary_said,
        &format!(
            "in that turn the manager read the {n} reports ({read_back} found in its tool results) and ended with its summary"
        ),
    );

    // ---- the survey: four Claude Code sessions, what each was asked and last said, within the budget
    let mut all_sids = vec![mgr_sid.clone()];
    all_sids.extend(sids.iter().cloned());
    let head = |s: &str| {
        s.lines()
            .next()
            .unwrap_or("")
            .chars()
            .take(24)
            .collect::<String>()
    };
    let end = |s: &str| {
        let last = s.trim_end().lines().last().unwrap_or("");
        let n = last.chars().count();
        last.chars().skip(n.saturating_sub(24)).collect::<String>()
    };
    let mut asked_said: Vec<AskedSaid> = vec![(
        mgr_sid.clone(),
        plan.mgr.clone(),
        head(&plan.summary),
        end(&plan.summary),
    )];
    for (k, w) in plan.workers.iter().enumerate() {
        if let Some(s) = sids.get(k) {
            asked_said.push((s.clone(), w.token.clone(), head(&w.report), end(&w.report)));
        }
    }
    // the survey reads what the sessions publish (each Claude Code footer's own read of its
    // transcript), a moment behind the transcript: read it until all four show what they were asked
    // and last said (a one-minute hang detector), and say how long after the summary was drawn
    let mut survey = door.ctl(&["survey"])?;
    let survey_settled = wait_for(Duration::from_secs(60), || {
        if survey_shown(&survey.stdout, &asked_said) == all_sids.len() {
            return true;
        }
        if let Ok(again) = door.ctl(&["survey"]) {
            survey = again;
        }
        false
    })?;
    let survey_wait = now() - t_end;
    let text = survey.stdout.clone();
    let session_lines: Vec<&str> = text.lines().filter(|l| l.starts_with("s-")).collect();
    let shown = survey_shown(&text, &asked_said);
    let long = session_lines
        .iter()
        .filter(|l| l.chars().count() > 110)
        .count();
    c.check(
        survey.code == 0
            && session_lines.len() == all_sids.len()
            && shown == all_sids.len()
            && long == 0
            && text.chars().count() <= 24_000,
        &format!(
            "survey: {} session lines, {shown} of the 4 Claude Code sessions with what each was asked and last said ({}), {} chars (budget 24000), {long} session lines past 110",
            session_lines.len(),
            if survey_settled {
                format!("all shown by a read {survey_wait:.1} s after the summary was drawn")
            } else {
                "not all shown within 60 s".to_string()
            },
            text.chars().count()
        ),
    );
    if c.bad > 0 || shown != all_sids.len() {
        for l in text.lines() {
            println!("    | {l}");
        }
    }
    let json = door.ctl(&["survey", "--json"])?;
    let records = J::parse(&json.stdout).ok();
    let sessions: Vec<J> = records
        .as_ref()
        .and_then(J::as_arr)
        .unwrap_or_default()
        .iter()
        .flat_map(|i| {
            i.get("sessions")
                .and_then(J::as_arr)
                .unwrap_or_default()
                .to_vec()
        })
        .collect();
    let field = |s: &J, k: &str| s.get(k).and_then(J::as_str).unwrap_or("").to_string();
    let json_ok = asked_said
        .iter()
        .filter(|(sid, asked, head, end)| {
            // A finished turn's record keeps its end when the server cut it.
            let said = |s: &J| {
                let said = field(s, "said");
                said.contains(head.as_str()) || said.contains(end.as_str())
            };
            sessions.iter().any(|s| {
                field(s, "sid") == *sid
                    && field(s, "program") == "claude"
                    && field(s, "prompt").contains(asked.as_str())
                    && said(s)
            })
        })
        .count();
    c.check(
        json.code == 0 && json_ok == all_sids.len(),
        &format!(
            "survey --json: {json_ok} of the 4 records are claude with their prompt and last words ({} records)",
            sessions.len()
        ),
    );

    // ---- egress: nothing external, and the witness saw the four Claude Codes talk to the stub
    let egress = fs::read_to_string(run.run.join("egress.log")).unwrap_or_default();
    let external = egress.lines().filter(|l| l.contains("EXTERNAL")).count();
    let to_stub = format!("127.0.0.1:{}", run.port);
    let mut clients: Vec<String> = fs::read_to_string(run.run.join("sockets.log"))
        .unwrap_or_default()
        .lines()
        .filter(|l| {
            // `<time> <cmd>[<pid>] <proto> <kind> <endpoint>` (the witness's line)
            let w: Vec<&str> = l.split_whitespace().collect();
            w.get(3) == Some(&"conn") && w.get(4) == Some(&to_stub.as_str())
        })
        .filter_map(|l| l.split_whitespace().nth(1).map(str::to_string))
        .collect();
    clients.sort();
    clients.dedup();
    c.check(
        external == 0 && clients.len() >= all_sids.len(),
        &format!(
            "egress: no external endpoint ({external}); the witness saw {} processes connect to the stub ({})",
            clients.len(),
            clients.join(" ")
        ),
    );

    // ---- the run on one clock
    if let (Some(last), Some((_, wr))) = (done_at.iter().copied().reduce(f64::max), wake)
        && let Some(w_ts) = wr.ts
    {
        println!(
            "  await     the last done pushed -> the manager's wake row {}",
            secs(wall_ms(last), w_ts)
        );
    }
    let spawn_results: Vec<i64> = mgr
        .iter()
        .filter(|r| {
            r.result
                .as_deref()
                .is_some_and(|t| t.trim().starts_with("OK s-"))
        })
        .filter_map(|r| r.ts)
        .collect();
    if let (Some(first), Some(last)) = (spawn_results.first(), spawn_results.last()) {
        println!(
            "  spawns    prompt -> first OK <sid> {}; first -> third {}",
            secs(wall_ms(t_prompt), *first),
            secs(*first, *last)
        );
    }
    println!(
        "  total     prompt -> summary drawn {:.1} s",
        t_end - t_prompt
    );
    if let Some(bin) = upgrade {
        let workers: Vec<(String, String)> = sids
            .iter()
            .zip(&plan.workers)
            .map(|(s, w)| (s.clone(), w.report.clone()))
            .collect();
        upgrade_drill(&run, &door, &all_sids, &workers, bin, &mut c)?;
    }
    if c.bad > 0 {
        let mut shown: Vec<String> = sids.clone();
        for (s, _) in &created {
            if !shown.contains(s) {
                shown.push(s.clone());
            }
        }
        keep_worker_texts(&run, &door, &shown);
    }
    if c.bad == 0 {
        println!("workers: PASS ({} checks)", c.n);
        Ok(Verdict::Pass)
    } else {
        println!("workers: FAIL ({} of {} checks)", c.bad, c.n);
        Ok(Verdict::Failed)
    }
}

/// `workers [tag] [--keep] [--early] [--upgrade <newer-claude-bin>]`.
pub fn cmd_workers(args: &[String]) -> Res {
    let usage = |m: &str| Fail::usage(format!("workers: {m}"));
    let (mut tag, mut keep, mut upgrade) = (None::<String>, false, None::<String>);
    let mut early = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--keep" => keep = true,
            "--early" => early = true,
            "--upgrade" => {
                let bin = it
                    .next()
                    .ok_or_else(|| usage("--upgrade needs the newer Claude Code binary"))?;
                upgrade = Some(crate::paths::from_caller(bin));
            }
            "-h" | "--help" => {
                print!("{}", crate::USAGE);
                return Ok(0);
            }
            s if s.starts_with('-') => {
                return Err(usage(&format!(
                    "unknown option '{s}' (--keep, --early, --upgrade <newer-claude-bin>)"
                )));
            }
            s => tag = Some(s.to_string()),
        }
    }
    let tag = tag.unwrap_or_else(|| format!("workers-{}", std::process::id()));
    if !crate::paths::valid_tag(&tag) {
        return Err(usage(&format!(
            "bad tag '{tag}' (letters, digits, _ and -, at most 24)"
        )));
    }
    if is_run(&tag) {
        return Err(usage(&format!(
            "lab '{tag}' is up: workers brings up a lab of its own (another tag, or {LAB} down {tag})"
        )));
    }
    stop::install();
    // the folder trust seeded: the proof is the workers, not the dialog (the harness answers it, `selftest`)
    let mut u = UpOpts::from_env();
    u.trust = true;
    // a lab of its own ends with this run, unless it is kept
    u.lifeline = !keep;
    if let Err(f) = crate::lab::up(&tag, &u) {
        if let Some(m) = &f.msg {
            eprintln!("aterm-lab: {m}");
        }
        if f.stopped.is_some() {
            return Err(f);
        }
        eprintln!("aterm-lab: workers: COULD NOT RUN: the lab did not come up");
        return Ok(3);
    }
    let verdict = run_workers(&tag, upgrade.as_deref(), early);
    if keep {
        println!("workers: lab '{tag}' left up ({LAB} down {tag})");
    } else if let Ok(k) = crate::lab::down(&tag, false) {
        println!("lab '{tag}' is down; logs kept in {}", k.display());
    }
    match verdict? {
        Verdict::Pass => Ok(0),
        Verdict::Failed => Ok(1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_iso_stamp_is_read_as_unix_milliseconds() {
        assert_eq!(iso_ms("1970-01-01T00:00:00.000Z"), Some(0));
        assert_eq!(iso_ms("2026-10-16T10:24:42.738Z"), Some(1_792_146_282_738));
        assert_eq!(iso_ms("2000-03-01T00:00:01Z"), Some(951_868_801_000));
        assert_eq!(iso_ms("2026-10-16 10:24:42"), None);
    }

    #[test]
    fn the_script_keys_each_conversation_and_the_stub_plays_it() {
        use crate::stub::{ToolTurn, tool_turn};
        let plan = Plan::new("0badc0de");
        let script = plan.script();
        let req =
            |msgs: &[J]| J::obj([("messages", J::Arr(msgs.to_vec()))]).dump(crate::pyjson::COMPACT);
        let user = |t: &str| J::obj([("role", J::s("user")), ("content", J::s(t))]);
        let used = J::obj([
            ("role", J::s("assistant")),
            (
                "content",
                J::Arr(vec![J::obj([("type", J::s("tool_use"))])]),
            ),
        ]);
        let result = |t: &str| {
            J::obj([
                ("role", J::s("user")),
                (
                    "content",
                    J::Arr(vec![J::obj([
                        ("type", J::s("tool_result")),
                        ("content", J::s(t)),
                    ])]),
                ),
            ])
        };
        let said = |t: &str| J::obj([("role", J::s("assistant")), ("content", J::s(t))]);
        // the manager: three spawns, each the task on stdin byte for byte
        let mut m = vec![user(&plan.prompt)];
        for (k, w) in plan.workers.iter().enumerate() {
            match tool_turn(req(&m).as_bytes(), &script) {
                Some(ToolTurn::Call(name, input, step)) => {
                    assert_eq!((name.as_str(), step), ("Bash", k));
                    assert_eq!(
                        input.get("command").and_then(J::as_str),
                        Some(Plan::spawn_command(w).as_str())
                    );
                }
                other => panic!("spawn {k}: {other:?}"),
            }
            m.push(used.clone());
            m.push(result(&format!("OK s-{k}{k}{k}a")));
        }
        // the background wait names the sids the spawns printed, and the turn then ends with text
        match tool_turn(req(&m).as_bytes(), &script) {
            Some(ToolTurn::Call(_, input, 3)) => {
                assert_eq!(
                    input.get("command").and_then(J::as_str),
                    Some("aterm ctl await task @s-000a,@s-111a,@s-222a all since=0")
                );
                assert!(input.get("run_in_background").is_some_and(J::truthy));
            }
            other => panic!("await: {other:?}"),
        }
        m.push(used.clone());
        m.push(result("Command running in background with ID: b1"));
        assert_eq!(
            tool_turn(req(&m).as_bytes(), &script),
            Some(ToolTurn::Say(plan.waiting.clone(), 4))
        );
        // the wake opens the next turn: each result, then the summary
        m.push(said(&plan.waiting));
        m.push(user(
            "<task-notification><status>completed</status></task-notification>",
        ));
        match tool_turn(req(&m).as_bytes(), &script) {
            Some(ToolTurn::Call(_, input, 5)) => assert_eq!(
                input.get("command").and_then(J::as_str),
                Some("aterm ctl @s-000a result")
            ),
            other => panic!("result: {other:?}"),
        }
        for k in 0..3 {
            m.push(used.clone());
            m.push(result(&plan.workers[k].report));
        }
        assert_eq!(
            tool_turn(req(&m).as_bytes(), &script),
            Some(ToolTurn::Say(plan.summary.clone(), 8))
        );
        // a worker's conversation is its own: one Bash, then its report and the marker
        let w = &plan.workers[1];
        let task_row = user(&format!("{}\n\n(Started by `s-m` …)", w.task));
        assert!(matches!(
            tool_turn(req(std::slice::from_ref(&task_row)).as_bytes(), &script),
            Some(ToolTurn::Call(n, _, 0)) if n == "Bash"
        ));
        assert_eq!(
            tool_turn(
                req(&[task_row, used, result("2 w2.out")]).as_bytes(),
                &script
            ),
            Some(ToolTurn::Say(format!("{}\n{MARKER}", w.report), 1))
        );
        // no token is another's: a worker's task names no other conversation's key
        for w in &plan.workers {
            assert!(!w.task.contains(&plan.mgr));
            assert!(
                plan.workers
                    .iter()
                    .filter(|o| w.task.contains(&o.token))
                    .count()
                    == 1
            );
            assert!(!w.task.lines().any(|l| l == "EOF"));
            assert_eq!(w.report.trim(), w.report, "the result reader trims");
        }
    }

    #[test]
    fn transcript_rows_give_the_prompt_the_tool_calls_and_their_results() {
        let text = [
            r#"{"type":"mode"}"#,
            r#"{"type":"user","timestamp":"2026-10-16T10:24:42.738Z","message":{"role":"user","content":"LABW1-x: go\n\n(Started by `s-m`)"}}"#,
            r#"{"type":"assistant","timestamp":"2026-10-16T10:24:42.836Z","message":{"role":"assistant","content":[{"type":"tool_use","name":"Bash","input":{"command":"ls"}}]}}"#,
            r#"{"type":"user","timestamp":"2026-10-16T10:24:42.916Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"OK s-0f0a"}]}}"#,
            r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"OK s-0f0a"}]}}"#,
            r#"{"type":"api-request-blob","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"OK s-0f0a"}]}}"#,
            "not json",
            r#"{"type":"assistant","timestamp":"2026-10-16T10:24:42.952Z","message":{"role":"assistant","content":[{"type":"text","text":"done"}]}}"#,
        ]
        .join("\n");
        let r = rows(&text);
        assert_eq!(r.len(), 7);
        // a request blob copies a message: it is no conversation row
        assert!(r[5].result.is_none() && r[5].kind == "api-request-blob");
        let p = first_prompt(&r).unwrap();
        assert_eq!(p.text, "LABW1-x: go\n\n(Started by `s-m`)");
        assert_eq!(p.ts, Some(1_792_146_282_738));
        assert_eq!(
            r[2].tool_use.as_ref().map(|(n, _)| n.as_str()),
            Some("Bash")
        );
        // one tool result written twice is one spawn
        assert_eq!(spawned(&r), vec!["s-0f0a".to_string()]);
        assert_eq!(r[6].text, "done");
        // a tool result is no prompt
        assert!(first_prompt(&r[3..4]).is_none());
    }

    #[test]
    fn the_pushes_name_each_session_by_its_ack() {
        let lines: Vec<(f64, String)> = [
            "sub 0 s-m",
            "EVENT 0 agent idle rev=3",
            "sub 1 s-w",
            "EVENT * session-created s-w",
            "EVENT 1 task rev=1 state=working attention=0",
            "EVENT 1 task rev=2 state=done attention=0 why=marker",
            "EVENT 7 task rev=1 state=stalled attention=1 why=shell",
            "GAP 1 events-dropped=2",
        ]
        .iter()
        .enumerate()
        .map(|(i, l)| (f64::from(u32::try_from(i).unwrap()), (*l).to_string()))
        .collect();
        let p = pushed(&lines);
        assert_eq!(p.created, vec![("s-w".to_string(), 3.0)]);
        assert_eq!(p.tasks.len(), 3);
        assert_eq!(
            p.tasks[1],
            (
                "s-w".into(),
                5.0,
                "rev=2 state=done attention=0 why=marker".into()
            )
        );
        assert_eq!(kv(&p.tasks[1].2, "why"), Some("marker"));
        // a move of a session no ack named is kept, and named by its local id
        assert_eq!(p.tasks[2].0, "local:7");
        assert_eq!(p.gaps.len(), 1);
        assert_eq!(
            kv("OK task rev=2 state=done result_bytes=15", "result_bytes"),
            Some("15")
        );
        assert_eq!(kv("OK task rev=2", "state"), None);
    }

    #[test]
    fn the_upgrade_column_is_read_off_the_sessions_own_ls_row() {
        let ls = "0 0 s-m - alive t program=claude upgrade=done/2.1.301/-/40s task=-\n\
                  0 1 s-w s-m alive t program=claude upgrade=pending/2.1.301/settling/5s task=done";
        assert_eq!(upgrade_of(ls, "s-m"), "done/2.1.301/-/40s");
        assert_eq!(upgrade_of(ls, "s-w"), "pending/2.1.301/settling/5s");
        // the parent column names s-m too, and is not its row
        assert_eq!(upgrade_of("0 1 s-w s-m alive upgrade=-", "s-m"), "");
    }

    #[test]
    fn a_notification_names_its_task_and_output_file() {
        let t = "<task-notification>\n<task-id>b1</task-id>\n<output-file>/private/tmp/x/b1.output</output-file>\n<status>completed</status>";
        assert_eq!(notification_field(t, "task-id"), Some("b1"));
        assert_eq!(
            notification_field(t, "output-file"),
            Some("/private/tmp/x/b1.output")
        );
        assert_eq!(
            notification_field("<status>completed</status>", "output-file"),
            None
        );
    }

    /// The await's output is read only where Claude Code writes the task's
    /// output for the manager's own conversation under the temp root the lab
    /// gave it, a path the lab builds: never wherever the notification's text
    /// points.
    #[test]
    fn the_await_output_is_read_where_the_managers_conversation_writes_it() {
        let root = Path::new("/s/runs/t/scratch/tmp.Ab12Cd");
        let transcript = Path::new("/s/home/.claude/projects/-private-tmp-at-Ab12-proj/5f0c.jsonl");
        let uid = crate::sys::getuid();
        assert_eq!(
            task_output_path(root, transcript, "b1x2"),
            Some(PathBuf::from(format!(
                "/s/runs/t/scratch/tmp.Ab12Cd/claude-{uid}/-private-tmp-at-Ab12-proj/5f0c/tasks/b1x2.output"
            )))
        );
        // NEGATIVE CONTROL: a task id that would leave the tasks folder names nothing
        for id in ["", "../x", "a/b", "..", "b1 x"] {
            assert_eq!(task_output_path(root, transcript, id), None, "{id:?}");
        }
        // ... nor does a transcript whose session or project is no folder of its own
        for t in [
            "/s/home/.claude/projects/p/...jsonl",
            "/s/home/.claude/projects/p/..jsonl",
            "/s/home/.claude/projects/../5f0c.jsonl",
            "5f0c.jsonl",
        ] {
            assert_eq!(task_output_path(root, Path::new(t), "b1x2"), None, "{t:?}");
        }
    }

    #[test]
    fn a_bounded_read_takes_a_short_file_and_refuses_a_long_one() {
        let d = std::fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!("lab-workers-read-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("b1.output");
        std::fs::write(&f, "@s-1 rev=2 state=done\n").unwrap();
        assert_eq!(
            read_bounded(&f, 64).as_deref(),
            Ok("@s-1 rev=2 state=done\n")
        );
        // NEGATIVE CONTROL: one byte past the bound is not read at all
        assert!(read_bounded(&f, 21).is_err());
        let link = d.join("link.output");
        std::os::unix::fs::symlink(&f, &link).unwrap();
        assert!(read_bounded(&link, 64).is_err(), "a link is not followed");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_survey_shows_a_session_only_with_what_it_was_asked_and_said() {
        let text = "== host pid 1 0.105.0 · 2 sessions trust=screen\n\
            s-m claude idle 0s | ✳ Claude Code\n\
            \x20   asked: LABMGR-x: start three workers\n\
            \x20   said: Summary: all three\n\
            s-w claude idle 0s task=done/2 | ✳ Claude Code\n\
            \x20   asked: LABW1-x: write the file\n\
            s-z bash running sleep\n";
        let want = |sid: &str, asked: &str, said: &str| {
            (
                sid.to_string(),
                asked.to_string(),
                said.to_string(),
                "the end of it".to_string(),
            )
        };
        assert_eq!(
            survey_shown(text, &[want("s-m", "LABMGR-x", "Summary: all")]),
            1
        );
        // A finished turn's said cut keeping its end shows by its end.
        let cut = text.replace(
            "    said: Summary: all three",
            "    said: …and the end of it",
        );
        assert_eq!(
            survey_shown(&cut, &[want("s-m", "LABMGR-x", "Summary: all")]),
            1
        );
        // NEGATIVE CONTROLS: no `said:` yet (the footer's read behind the transcript), another
        // session's words, and a session that is no Claude Code
        assert_eq!(
            survey_shown(text, &[want("s-w", "LABW1-x", "Worker one")]),
            0
        );
        assert_eq!(
            survey_shown(text, &[want("s-w", "LABW1-x", "Summary: all")]),
            0
        );
        assert_eq!(survey_shown(text, &[want("s-z", "", "")]), 0);
    }

    #[test]
    fn a_record_never_stalled_moved_through_every_rev_working_and_done() {
        let timeline = "OK 4\n\
            event 1 t=5 kind=spawned pid=7\n\
            event 2 t=6 kind=task-change rev=1 state=working attention=0\n\
            event 3 t=9 kind=agent-change agent=busy\n\
            event 4 t=12 kind=task-change rev=2 state=done attention=0 why=marker\n";
        let moves = task_moves(timeline);
        assert_eq!(
            moves,
            vec![(1, "working".to_string()), (2, "done".to_string())]
        );
        assert!(never_stalled(&moves, Some(2)));
        // NEGATIVE CONTROLS: a stall, a rev the timeline lost, a rev past the newest recorded,
        // and no record read
        let stalled = [(1, "working".to_string()), (2, "stalled".to_string())];
        assert!(!never_stalled(&stalled, Some(2)));
        assert!(!never_stalled(&moves[1..], Some(2)), "rev 1 missing");
        assert!(!never_stalled(&moves, Some(3)), "rev 3 never recorded");
        assert!(!never_stalled(&moves, None));
    }
}
