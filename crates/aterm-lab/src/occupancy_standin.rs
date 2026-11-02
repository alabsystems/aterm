// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The `claude` of `occupancy-wake-standin`: `occupancy-wake-claude` with the lab's own program in
//! the agent's place, so all of the harness's side runs in the lab's jail with no real agent and
//! no API.
//!
//! It does what a real run's Claude Code and the stub do between them, and no more. It takes the
//! turn the lab types and plays the tool script the scenario set (`tool.json`, [`steps`]): a
//! `Bash` step runs in its own process tree, as a real agent's tool would, a `Workflow` step
//! leaves it waiting, and each `text` is a reply. It paints the rows `aterm-phase` reads from real
//! captures ([`frame`]), echoes what is typed into its composer, and takes Enter.
//!
//! So a pass says the harness's side holds: the verbs, the file atom, the door, the rows and the
//! judge. It says nothing of a real Claude Code, which `occupancy-wake-claude` is for, and its run
//! is never promoted as a capture of one (`tape::summary_refusal`, [`SUMMARY_KEY`]).

use std::collections::VecDeque;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_uds::spawnfd::Stdio;

use crate::pyjson::J;
use crate::sys;
use crate::util::scratch_text;

/// The folder under the lab's run directory that holds the agent under test.
const DIR: &str = "standin";

/// The file beside its `claude` that says it is this stand-in (`main`'s dispatch on `argv[0]`).
const MARK: &str = "occupancy-wake";

/// A summary key a stand-in's run carries ([`crate::tape::run_failed`]'s neighbour): it is no
/// capture of a real agent.
pub const SUMMARY_KEY: &str = "standin";

/// Its log under the lab's scratch folder, which `down` keeps: a failed turn's only witness.
pub const LOG: &str = "standin.log";

/// The version it answers: the lab reads one, and no reader keys on it.
const VERSION: &str = "2.1.300 (Claude Code stand-in of aterm-lab's occupancy-wake-standin)";

/// How long a turn paints live, so the loop reads a turn run.
const LIVE: Duration = Duration::from_millis(1_400);

/// How often a live turn repaints.
const TICK: Duration = Duration::from_millis(200);

/// The screen's fallback size.
const SIZE: (usize, usize) = (144, 50);

/// Lay the stand-in under the run directory `run`, which outlives a lab: a `claude` link to the
/// lab's own program `bin`, over the last lab's, and the mark beside it. Its path.
///
/// `bin` is the program itself and no copy of it in the run: the debug build's harness drives a
/// stand-in only by the file `instance::standins_seam` names, and leaves any other image observed.
///
/// # Errors
/// The folder, the link or the mark could not be made.
pub fn lay(run: &Path, bin: &Path) -> std::io::Result<PathBuf> {
    let dir = run.join(DIR);
    std::fs::create_dir_all(&dir)?;
    let claude = dir.join("claude");
    let _ = std::fs::remove_file(&claude);
    std::os::unix::fs::symlink(bin, &claude)?;
    std::fs::write(dir.join(MARK), "")?;
    Ok(claude)
}

/// Whether `argv0`, a program called `claude`, is the stand-in [`lay`] laid. The lab starts it by
/// the absolute path [`lay`] returns, so a relative `argv0` (a bare `claude` found on `PATH`) is
/// never it, whatever the working directory holds.
#[must_use]
pub fn configured(argv0: &str) -> bool {
    let argv0 = Path::new(argv0);
    argv0.is_absolute() && argv0.parent().is_some_and(|dir| dir.join(MARK).is_file())
}

/// One step of the agent's own thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// A `Bash` tool call: its command.
    Bash(String),
    /// A `Workflow` tool call: the agent then waits on it.
    Workflow,
    /// A reply.
    Text(String),
}

/// The agent's own steps of a tool script (`verbs::set_tool`'s), in order. A step marked `when`
/// is a subagent's, which the stand-in has none of, and a step of another name is a tool the
/// scenario does not use.
#[must_use]
pub fn steps(script: &J) -> VecDeque<Step> {
    script
        .as_arr()
        .unwrap_or_default()
        .iter()
        .filter(|step| step.get("when").is_none())
        .filter_map(|step| {
            if let Some(text) = step.get("text").and_then(J::as_str) {
                return Some(Step::Text(text.to_string()));
            }
            match step.get("name").and_then(J::as_str) {
                Some("Bash") => step
                    .get("input")
                    .and_then(|input| input.get("command"))
                    .and_then(J::as_str)
                    .map(|command| Step::Bash(command.to_string())),
                Some("Workflow") => Some(Step::Workflow),
                _ => None,
            }
        })
        .collect()
}

/// What the screen draws, under the transcript's rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Drawn {
    /// At rest, nothing of its own running.
    Idle,
    /// A live turn, at its tick.
    Live(u64),
    /// Its turn over, waiting on the workflow it started this many seconds ago: the progress
    /// row's clock moves the screen under the door, as a real footer's does.
    Waiting(u64),
}

/// `text`'s rows at `cols`: a first row after `head`, the rest indented to match.
fn wrapped(head: &str, text: &str, cols: usize) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let width = cols.saturating_sub(2).max(1);
    let mut rows: Vec<String> = chars
        .chunks(width)
        .enumerate()
        .map(|(at, part)| {
            let start = if at == 0 { head } else { "  " };
            format!("{start}{}", part.iter().collect::<String>())
        })
        .collect();
    if rows.is_empty() {
        rows.push(head.to_string());
    }
    rows
}

/// The screen: Claude Code's rows as `aterm-phase` reads them, at `cols` by `lines`, with `prompt`
/// and `said` the transcript's last turn and `draft` the composer's text, between the frame's two
/// rules above the footer [`Drawn`] asks for.
#[must_use]
pub fn frame(
    drawn: Drawn,
    (cols, lines): (usize, usize),
    prompt: &str,
    said: &str,
    draft: &str,
) -> Vec<String> {
    let rule = "─".repeat(cols);
    let mut top = vec![
        String::new(),
        " ▐▛███▛█   Claude Code v2.1.300".to_string(),
        "▝▜██████▀  Opus 4.6 · Claude Max".to_string(),
        String::new(),
    ];
    if !prompt.is_empty() {
        top.extend(wrapped("❯ ", prompt, cols));
        top.push(String::new());
    }
    if !said.is_empty() {
        top.extend(wrapped("⏺ ", said, cols));
    }
    let mut bottom = Vec::new();
    match drawn {
        Drawn::Idle => {}
        Drawn::Live(tick) => {
            const GLYPHS: [&str; 6] = ["✢", "✳", "✶", "✻", "✽", "✻"];
            let glyph = GLYPHS[usize::try_from(tick % 6).unwrap_or(0)];
            bottom.push(format!("{glyph} Working… ({}s · ↓ 1.2k tokens)", tick / 4));
            bottom.push(String::new());
        }
        Drawn::Waiting(_) => bottom.push("✻ Waiting for 1 dynamic workflow to finish".to_string()),
    }
    bottom.push(rule.clone());
    bottom.extend(wrapped("❯ ", draft, cols));
    bottom.push(rule);
    bottom.push(match drawn {
        Drawn::Idle => "  ⏵⏵ bypass permissions on (shift+tab to cycle)".to_string(),
        Drawn::Live(_) => {
            "  ⏵⏵ bypass permissions on (shift+tab to cycle) · esc to interrupt".to_string()
        }
        Drawn::Waiting(_) => {
            "  ⏵⏵ bypass permissions on (shift+tab to cycle) · ← for agents".to_string()
        }
    });
    if let Drawn::Waiting(seconds) = drawn {
        bottom.push(format!(
            "  ◯ occupancy-wake-lab  Two sleeps the wake lab waits on … 0/2 agents done · {}m {}s",
            seconds / 60,
            seconds % 60
        ));
    }
    while top.len() + bottom.len() < lines {
        top.push(String::new());
    }
    let over = (top.len() + bottom.len()).saturating_sub(lines);
    top.drain(..over.min(top.len()));
    top.extend(bottom);
    top
}

/// Where the caret stands on `rows`: after `draft`, in the composer below the last rule but one.
fn caret(rows: &[String], draft: &str, cols: usize) -> (usize, usize) {
    let width = cols.saturating_sub(2).max(1);
    let typed = draft.chars().count();
    let last = typed.saturating_sub(1) / width;
    let first = rows.iter().rposition(|r| r.starts_with('❯')).unwrap_or(0);
    (first + last, typed.saturating_sub(last * width) + 2)
}

/// One keystroke run read from the terminal applied to `draft`: the lines it submitted, in order.
/// A control sequence is skipped whole, Backspace takes a character and Ctrl-U clears.
#[must_use]
pub fn typed(draft: &mut String, input: &str) -> Vec<String> {
    let mut submitted = Vec::new();
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' | '\n' => {
                let line = draft.trim().to_string();
                draft.clear();
                if !line.is_empty() {
                    submitted.push(line);
                }
            }
            '\x7f' | '\x08' => {
                draft.pop();
            }
            '\x15' => draft.clear(),
            '\x1b' => {
                if chars.next_if_eq(&'[').is_some() {
                    while chars.next().is_some_and(|c| !('@'..='~').contains(&c)) {}
                }
            }
            c if c.is_control() => {}
            c => draft.push(c),
        }
    }
    submitted
}

/// The agent: what it has drawn, what the script has left, and where it writes its log.
struct Agent {
    home: PathBuf,
    steps: Option<VecDeque<Step>>,
    waiting: Option<Instant>,
    prompt: String,
    said: String,
    draft: String,
    painted: Vec<String>,
}

impl Agent {
    /// The folder the lab scratches in, the script's and the log's: `$HOME`'s parent.
    fn scratch(&self) -> PathBuf {
        self.home
            .parent()
            .map_or_else(|| self.home.clone(), Path::to_path_buf)
    }

    /// Append `line` to [`LOG`].
    fn log(&self, line: &str) {
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.scratch().join(LOG))
        {
            let _ = writeln!(file, "{line}");
        }
    }

    fn at_rest(&self) -> Drawn {
        self.waiting.map_or(Drawn::Idle, |since| {
            Drawn::Waiting(since.elapsed().as_secs())
        })
    }

    /// Paint `drawn`, where it differs from the screen: a read of a screen that did not move
    /// keeps its generation, as the door's fences need.
    fn paint(&mut self, drawn: Drawn) {
        let size = sys::winsize(1).map_or(SIZE, |ws| {
            (
                usize::from(ws.ws_col).max(40),
                usize::from(ws.ws_row).max(16),
            )
        });
        let rows = frame(drawn, size, &self.prompt, &self.said, &self.draft);
        if rows == self.painted {
            return;
        }
        let (row, col) = caret(&rows, &self.draft, size.0);
        let mut out = String::new();
        for (at, text) in rows.iter().enumerate() {
            out.push_str(&format!("\x1b[{};1H\x1b[2K{text}", at + 1));
        }
        out.push_str(&format!("\x1b[{};{}H", row + 1, col + 1));
        let mut stdout = std::io::stdout().lock();
        let _ = stdout
            .write_all(out.as_bytes())
            .and_then(|()| stdout.flush());
        self.painted = rows;
    }

    /// Run `command` as the agent's Bash tool: its own child, so a declaration comes from the
    /// session's tree.
    fn bash(&self, command: &str) {
        let done = aterm_uds::spawnfd::command("/bin/sh")
            .args(["-c", command])
            .stdin(Stdio::null())
            .output();
        match done {
            Ok(out) => self.log(&format!(
                "bash {command:?}: exit={:?} stdout={:?} stderr={:?}",
                out.status.code(),
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr),
            )),
            Err(e) => self.log(&format!("bash {command:?}: {e}")),
        }
    }

    /// One submitted line: a live turn, then the script's steps up to its next reply.
    fn turn(&mut self, line: String, signals: &sys::SignalPipe) -> bool {
        self.log(&format!("turn {line:?}"));
        self.prompt = line;
        let ticks = LIVE.as_millis() / TICK.as_millis().max(1);
        for tick in 0..ticks {
            self.paint(Drawn::Live(u64::try_from(tick).unwrap_or(0)));
            let mut fds = [sys::pollfd(signals.read_fd)];
            let _ = sys::poll(&mut fds, i32::try_from(TICK.as_millis()).unwrap_or(200));
            if ended(signals) {
                return false;
            }
        }
        if self.steps.is_none() {
            let script = scratch_text(&self.scratch().join("tool.json"));
            self.steps = J::parse(&script).ok().as_ref().map(steps);
        }
        while let Some(step) = self.steps.as_mut().and_then(VecDeque::pop_front) {
            match step {
                Step::Bash(command) => self.bash(&command),
                Step::Workflow => self.waiting = Some(Instant::now()),
                Step::Text(text) => {
                    self.said = text;
                    break;
                }
            }
        }
        true
    }
}

/// Whether a signal asked the program to end.
fn ended(signals: &sys::SignalPipe) -> bool {
    signals
        .drain()
        .iter()
        .any(|s| matches!(*s, libc::SIGTERM | libc::SIGINT | libc::SIGHUP))
}

/// The stand-in's `main`.
pub fn claude_main(args: &[String]) -> i32 {
    if args.iter().any(|a| a == "--version" || a == "-v") {
        println!("{VERSION}");
        return 0;
    }
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return 1;
    };
    let Ok(signals) = sys::SignalPipe::install(&[libc::SIGTERM, libc::SIGINT, libc::SIGHUP]) else {
        return 1;
    };
    let _raw = sys::RawMode::enter(0);
    {
        let mut out = std::io::stdout().lock();
        let _ = out
            .write_all(b"\x1b[?1049h\x1b[2J\x1b[H")
            .and_then(|()| out.flush());
    }
    let mut agent = Agent {
        home,
        steps: None,
        waiting: None,
        prompt: String::new(),
        said: String::new(),
        draft: String::new(),
        painted: Vec::new(),
    };
    loop {
        agent.paint(agent.at_rest());
        let mut fds = [sys::pollfd(0), sys::pollfd(signals.read_fd)];
        match sys::poll(&mut fds, 1_000) {
            Ok(0) => continue,
            Ok(_) => {}
            Err(_) => return 0,
        }
        if ended(&signals) {
            return 0;
        }
        if fds[0].revents == 0 {
            continue;
        }
        let mut buf = [0u8; 4096];
        let Ok(read) = sys::read(0, &mut buf) else {
            return 0;
        };
        if read == 0 {
            return 0;
        }
        let input = String::from_utf8_lossy(&buf[..read]).into_owned();
        for line in typed(&mut agent.draft, &input) {
            if !agent.turn(line, &signals) {
                return 0;
            }
        }
    }
}

#[cfg(test)]
#[path = "occupancy_standin_tests.rs"]
mod tests;
