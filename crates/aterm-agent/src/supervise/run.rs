// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The supervisor loop over the control verbs: wait for the worker's turn to
//! end ([`Session::await_turn`]), then either approve a read-only Bash prompt
//! itself or hand the screen to the manager ([`Session::supervise`]). Every
//! request goes through one [`Ctl`] seam — the `aterm-ctl` launcher the other
//! drive subcommands use in production, a scripted mock in the tests — and the
//! server features newer builds add (`text … tail=`, `await gone`, `key if=`)
//! are probed ONCE and remembered, so the loop runs against an older host too.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::classify::{DEFAULT_PYTHON_ALLOW, Verdict, classify_command_with};
use super::phase::{Phase, composer_text, is_placeholder, worker_phase};
use super::prompt::{Prompt, PromptKind, parse_prompt, prompt_box_span};
use super::screen::{Screen, parse_text_json};

/// One `aterm-ctl` exchange: the exit code, stdout and stderr.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CtlReply {
    /// The client's exit code: 0 ok, 124 a timeout, 1 an error (`ERR …` on stderr).
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl CtlReply {
    pub fn ok(&self) -> bool {
        self.code == 0
    }
    pub fn timed_out(&self) -> bool {
        self.code == 124
    }
    /// The server does not know this form of the verb — an older build.
    pub fn usage_error(&self) -> bool {
        self.stderr.contains("ERR usage")
    }
    /// The `ERR …` line, without the client's `aterm-ctl: ` prefix.
    pub fn err_text(&self) -> &str {
        let line = self.stderr.trim();
        line.strip_prefix("aterm-ctl:")
            .map(str::trim)
            .unwrap_or(line)
    }
    /// A bare `ERR` with nothing after it — what a build that predates a verb's
    /// leading option answers when the option is read as a key name (measured:
    /// aterm 0.81.0 answers `key if=Do.you.want.to.proceed 1` and `key
    /// nosuchkey` alike with `aterm-ctl: ERR`, exit 1, stdout empty).
    pub fn bare_err(&self) -> bool {
        !self.ok() && self.err_text() == "ERR"
    }
    /// The server does not know this form: a usage line or a bare `ERR`.
    pub fn unknown_form(&self) -> bool {
        self.usage_error() || self.bare_err()
    }
    /// Whether the reply is `ERR <what>` (`busy sink`, `halted`, …).
    pub fn is_err(&self, what: &str) -> bool {
        !self.ok() && self.err_text().starts_with(&format!("ERR {what}"))
    }
    /// The `seq=<n>` an input verb stamps on its answer (`OK seq=31`,
    /// `OK skipped seq=31`): the content baseline at the write.
    pub fn seq(&self) -> Option<u64> {
        let rest = &self.stdout[self.stdout.find("seq=")? + "seq=".len()..];
        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        digits.parse().ok()
    }
    /// A guarded write that matched no row: `OK skipped seq=<n>` — nothing
    /// was written.
    pub fn skipped(&self) -> bool {
        self.ok() && self.stdout.trim_start().starts_with("OK skipped")
    }
}

/// The transport seam: run one control request against the target session.
pub trait Ctl {
    /// `args` is the request after any `@sid` selector, e.g. `["text", "--json"]`.
    fn call(&mut self, args: &[&str]) -> Result<CtlReply, String>;
}

impl Ctl for crate::CtlClient {
    fn call(&mut self, args: &[&str]) -> Result<CtlReply, String> {
        self.run_raw(args)
    }
}

/// Server features probed once per process (`None` = not yet asked).
#[derive(Debug, Default, Clone, Copy)]
pub struct Caps {
    /// `text --json tail=<n>` (a newer build sends only the last n rows).
    pub tail: Option<bool>,
    /// `await gone <re>`.
    pub gone: Option<bool>,
    /// `key if=<re> <key>` — the press happens only if a visible row matches.
    pub key_if: Option<bool>,
}

/// A turn's end: the phase it ended in and the screen it was read from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Turn {
    pub phase: Phase,
    pub screen: Screen,
    /// The budget ran out with the phase still busy.
    pub timed_out: bool,
}

/// `supervise`'s knobs.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SuperviseOpts {
    /// Approve (option 1) a Bash prompt whose command classifies read-only.
    pub auto_reads: bool,
    /// The wall-clock budget.
    pub max: Duration,
    /// `python3 <script>` globs that count as reads (empty = the default list).
    pub python_allow: Vec<String>,
    /// A notes file to append one line per action to.
    pub notes: Option<PathBuf>,
}

impl SuperviseOpts {
    fn allow(&self) -> Vec<String> {
        if self.python_allow.is_empty() {
            DEFAULT_PYTHON_ALLOW.iter().map(|s| s.to_string()).collect()
        } else {
            self.python_allow.clone()
        }
    }
}

/// How many rows a screen read asks for when the server can tail.
const TAIL_ROWS: &str = "40";
/// The longest single wait, so the budget is re-checked between waits.
const WAIT_STEP: Duration = Duration::from_secs(20);
/// The idle window that means "the screen settled".
const IDLE_MS: &str = "2000";
/// The busy footer whose LEAVING ends a turn (one token: the wire never quotes).
const BUSY_FOOTER: &str = "esc.to.interrupt";
/// The row `key if=` requires before pressing an option.
const PROCEED: &str = "Do.you.want.to.proceed";
/// How long the fallback press waits for the worker to take the digit before
/// looking for one that landed in the composer.
const STRAY_SETTLE: Duration = Duration::from_millis(2000);
/// `ERR busy sink` is transient (a spill ahead of the frame, another writer):
/// retried this many times, this far apart, before it is an error.
const BUSY_SINK_RETRIES: u32 = 3;
const BUSY_SINK_BACKOFF: Duration = Duration::from_millis(250);
/// The same read-only prompt is approved this many times; the next time it
/// comes back it is the manager's (a command that keeps failing and being
/// retried is not a read the supervisor should keep waving through).
const MAX_APPROVALS_OF_ONE_COMMAND: usize = 2;
/// How many rows the compact result prints when there is no box.
const RESULT_ROWS: usize = 28;

/// The exit code `supervise` / `await-turn` end with on a spent budget.
pub const EXIT_TIMEOUT: u8 = 124;

enum Wait {
    Latched,
    TimedOut,
    Unsupported,
}

/// What one guarded press did. `seq` is the content baseline the server
/// stamped on its answer (the screen read before it, when it stamped none):
/// the caller waits for the screen to move PAST it before looking again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Press {
    /// The `1` was written while the box was up.
    Pressed { seq: u64 },
    /// Nothing was written: the box was no longer on the screen at the check
    /// (or, on the fallback path, the digit landed in the composer and was
    /// backspaced — the prompt had resolved on its own).
    Skipped { seq: u64 },
}

/// One driven session: the transport, the target selector, the probed caps.
pub struct Session<'a, C: Ctl> {
    ctl: &'a mut C,
    sid: Option<String>,
    caps: Caps,
}

impl<'a, C: Ctl> Session<'a, C> {
    pub fn new(ctl: &'a mut C, sid: Option<String>) -> Self {
        Self {
            ctl,
            sid,
            caps: Caps::default(),
        }
    }

    /// The features the server turned out to have (for diagnostics).
    pub fn caps(&self) -> Caps {
        self.caps
    }

    fn call(&mut self, args: &[&str]) -> Result<CtlReply, String> {
        let mut full: Vec<&str> = Vec::with_capacity(args.len() + 1);
        if let Some(sid) = &self.sid {
            full.push(sid.as_str());
        }
        full.extend_from_slice(args);
        self.ctl.call(&full)
    }

    /// Read the screen: `text --json tail=40` where the server accepts it (probed
    /// once), else the full `text --json`.
    pub fn read_screen(&mut self) -> Result<Screen, String> {
        if self.caps.tail != Some(false) {
            let tail = format!("tail={TAIL_ROWS}");
            let r = self.call(&["text", "--json", &tail])?;
            if r.usage_error() {
                self.caps.tail = Some(false);
            } else if r.ok() {
                self.caps.tail = Some(true);
                return parse_text_json(&r.stdout);
            } else {
                return Err(format!("text --json {tail} failed: {}", r.stderr.trim()));
            }
        }
        let r = self.call(&["text", "--json"])?;
        if !r.ok() {
            return Err(format!("text --json failed: {}", r.stderr.trim()));
        }
        parse_text_json(&r.stdout)
    }

    fn wait(&mut self, cond: &[&str], step: Duration) -> Result<Wait, String> {
        let ms = step.as_millis().to_string();
        let mut args = vec!["await"];
        args.extend_from_slice(cond);
        args.push("timeout");
        args.push(&ms);
        let r = self.call(&args)?;
        if r.ok() {
            Ok(Wait::Latched)
        } else if r.timed_out() {
            Ok(Wait::TimedOut)
        } else if r.usage_error() {
            Ok(Wait::Unsupported)
        } else {
            Err(format!(
                "await {} failed: {}",
                cond.join(" "),
                r.stderr.trim()
            ))
        }
    }

    /// Block until the worker's phase is no longer busy, or `timeout` passes.
    ///
    /// The loop: wait for the screen to settle (`await idle 2000`, capped at
    /// 20 s a step); read; done unless busy; else wait for the next content
    /// change (`await seq <seq>`) and go again. Where the server knows `await
    /// gone`, the FIRST wait is the busy footer leaving — the signal that holds
    /// when a thinking worker sits static for seconds — falling back to idle on
    /// an older build.
    pub fn await_turn(&mut self, timeout: Duration) -> Result<Turn, String> {
        self.await_turn_from(timeout, true)
    }

    /// [`Self::await_turn`], with the `await gone` first wait optional: right
    /// after an approval the busy footer is not up YET, so its leaving is no
    /// signal — the settle wait is the first one.
    fn await_turn_from(&mut self, timeout: Duration, gone_first: bool) -> Result<Turn, String> {
        let deadline = Instant::now() + timeout;
        let mut first = gone_first;
        loop {
            let step = deadline
                .saturating_duration_since(Instant::now())
                .min(WAIT_STEP);
            let mut settled = false;
            if first && self.caps.gone != Some(false) {
                match self.wait(&["gone", BUSY_FOOTER], step)? {
                    Wait::Unsupported => self.caps.gone = Some(false),
                    _ => {
                        self.caps.gone = Some(true);
                        settled = true;
                    }
                }
            }
            if !settled {
                self.wait(&["idle", IDLE_MS], step)?;
            }
            first = false;
            let screen = self.read_screen()?;
            let phase = worker_phase(&screen.rows);
            if phase != Phase::Busy {
                return Ok(Turn {
                    phase,
                    screen,
                    timed_out: false,
                });
            }
            if Instant::now() >= deadline {
                return Ok(Turn {
                    phase,
                    screen,
                    timed_out: true,
                });
            }
            let seq = screen.seq.to_string();
            self.wait(&["seq", &seq], step)?;
        }
    }

    /// The loop. Returns the text to print and the exit code: `0` with the
    /// compact result when the worker needs the manager (a non-read prompt, a
    /// question, an idle composer); [`EXIT_TIMEOUT`] with `TIMEOUT` when the
    /// budget is spent.
    pub fn supervise(&mut self, opts: &SuperviseOpts) -> Result<(String, u8), String> {
        let deadline = Instant::now() + opts.max;
        let allow = opts.allow();
        let mut approved: Vec<String> = Vec::new();
        // The first wait of the next turn: the busy footer leaving, except
        // right after a press, when it is not up yet.
        let mut gone_first = true;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let turn = self.await_turn_from(remaining, gone_first)?;
            gone_first = true;
            if turn.timed_out {
                let mut out = String::from("TIMEOUT\n");
                out.push_str(&render_result(&turn, &allow));
                return Ok((out, EXIT_TIMEOUT));
            }
            if turn.phase == Phase::Prompt
                && opts.auto_reads
                && let Some(p) = parse_prompt(&turn.screen.rows)
                && p.kind == PromptKind::Bash
            {
                let v = classify_command_with(&p.command, &allow);
                let repeats = approved.iter().filter(|c| **c == p.command).count();
                let handed = if !v.read_only {
                    Some(v.reason.clone())
                } else if repeats >= MAX_APPROVALS_OF_ONE_COMMAND {
                    Some("the same prompt came back after two approvals".to_string())
                } else {
                    None
                };
                if let Some(why) = handed {
                    append_note(
                        opts.notes.as_deref(),
                        &format!("handed to the manager ({why}): {}", p.command),
                    )?;
                    return Ok((render_result(&turn, &allow), 0));
                }
                match self.press_one_guarded(&p, &turn.screen)? {
                    Press::Pressed { seq } => {
                        approved.push(p.command.clone());
                        append_note(
                            opts.notes.as_deref(),
                            &format!("approved read-only: {}", p.command),
                        )?;
                        // The box must LEAVE before the next look: `await gone`
                        // is level-triggered and a box shows no busy footer, so
                        // an immediate re-read of an unchanged screen would match
                        // the same box and press it again — the stray digit the
                        // guard exists to prevent.
                        if !self.moved_past(seq, deadline)? {
                            append_note(
                                opts.notes.as_deref(),
                                &format!(
                                    "handed to the manager (the box did not change after the press): {}",
                                    p.command
                                ),
                            )?;
                            let now = self.current_turn()?;
                            return Ok((render_result(&now, &allow), 0));
                        }
                        gone_first = false;
                        continue;
                    }
                    Press::Skipped { seq } => {
                        if seq == turn.screen.seq {
                            // The very screen we parsed, and the guard found no
                            // row: this box is not one the supervisor answers.
                            append_note(
                                opts.notes.as_deref(),
                                &format!(
                                    "handed to the manager (the guarded press matched no row): {}",
                                    p.command
                                ),
                            )?;
                            return Ok((render_result(&turn, &allow), 0));
                        }
                        append_note(
                            opts.notes.as_deref(),
                            &format!(
                                "nothing pressed, the box had left the screen: {}",
                                p.command
                            ),
                        )?;
                        continue;
                    }
                }
            }
            return Ok((render_result(&turn, &allow), 0));
        }
    }

    /// One read, classified.
    fn current_turn(&mut self) -> Result<Turn, String> {
        let screen = self.read_screen()?;
        Ok(Turn {
            phase: worker_phase(&screen.rows),
            screen,
            timed_out: false,
        })
    }

    /// Wait (one step, capped by `deadline`) for the content to move past
    /// `seq`: `true` when it did, `false` when the screen sat unchanged.
    fn moved_past(&mut self, seq: u64, deadline: Instant) -> Result<bool, String> {
        let step = deadline
            .saturating_duration_since(Instant::now())
            .min(WAIT_STEP);
        match self.wait(&["seq", &seq.to_string()], step)? {
            Wait::Latched => Ok(true),
            Wait::TimedOut => Ok(false),
            Wait::Unsupported => Err("await seq is not known to this host".to_string()),
        }
    }

    /// Press option 1 so that a prompt that resolved meanwhile never gets a
    /// stray `1` in the composer: `key if=Do.you.want.to.proceed 1` where the
    /// server has it (the check and the press happen under one lock; `OK
    /// skipped seq=<n>` means no row matched and nothing was written), else
    /// read → confirm the same box is still up → press → wait for the worker
    /// to take it → re-read → backspace a digit that landed in the composer.
    ///
    /// The capability probe is the reply: `OK …` proves the guard (pressed or
    /// skipped); a usage line or a bare `ERR` — what a build without `if=`
    /// answers, reading `if=…` as a key name — falls back for good; `ERR busy
    /// sink` is retried; every other `ERR` (`halted`, `no such session`,
    /// `badregex`) is an error, never an approval.
    fn press_one_guarded(&mut self, prompt: &Prompt, seen: &Screen) -> Result<Press, String> {
        if self.caps.key_if != Some(false) {
            let cond = format!("if={PROCEED}");
            let mut busy = 0;
            loop {
                let r = self.call(&["key", &cond, "1"])?;
                if r.ok() {
                    self.caps.key_if = Some(true);
                    let seq = r.seq().unwrap_or(seen.seq);
                    return Ok(if r.skipped() {
                        Press::Skipped { seq }
                    } else {
                        Press::Pressed { seq }
                    });
                }
                if r.unknown_form() {
                    self.caps.key_if = Some(false);
                    break;
                }
                if r.is_err("busy sink") && busy < BUSY_SINK_RETRIES {
                    busy += 1;
                    std::thread::sleep(BUSY_SINK_BACKOFF);
                    continue;
                }
                return Err(format!("key {cond} 1 failed: {}", r.err_text()));
            }
        }
        let now = self.read_screen()?;
        match parse_prompt(&now.rows) {
            Some(q) if q.command == prompt.command => {}
            _ => return Ok(Press::Skipped { seq: now.seq }),
        }
        let r = self.call(&["key", "1"])?;
        if !r.ok() {
            return Err(format!("key 1 failed: {}", r.err_text()));
        }
        let seq = r.seq().unwrap_or(now.seq);
        // Let the worker take the press before looking for a stray digit: the
        // first content change after it, bounded.
        self.wait(&["seq", &seq.to_string()], STRAY_SETTLE)?;
        let after = self.read_screen()?;
        if parse_prompt(&after.rows).is_none()
            && composer_text(&after.rows).as_deref() == Some("1")
            && !is_placeholder(&after.rows, after.cursor_col)
        {
            self.call(&["key", "backspace"])?;
            return Ok(Press::Skipped { seq: after.seq });
        }
        Ok(Press::Pressed { seq })
    }
}

/// The lines `phase` / `await-turn` print: the phase word, then for a prompt
/// its parsed kind, command, description, classification and options.
pub fn render_phase(turn: &Turn, python_allow: &[String]) -> String {
    let mut out = format!("{}\n", turn.phase.name());
    if turn.phase == Phase::Prompt
        && let Some(p) = parse_prompt(&turn.screen.rows)
    {
        out.push_str(&render_prompt(&p, python_allow));
    }
    out
}

fn render_prompt(p: &Prompt, python_allow: &[String]) -> String {
    let mut out = format!("kind {}\n", p.kind.name());
    if !p.command.is_empty() {
        out.push_str(&format!("command {}\n", p.command));
    }
    if !p.description.is_empty() && p.kind != PromptKind::Other {
        out.push_str(&format!("description {}\n", p.description));
    }
    if p.kind == PromptKind::Bash {
        let Verdict { read_only, reason } = classify_command_with(&p.command, python_allow);
        if read_only {
            out.push_str("classify read-only\n");
        } else {
            out.push_str(&format!("classify not-read-only {reason}\n"));
        }
    }
    for (n, text) in &p.options {
        out.push_str(&format!("option {n} {text}\n"));
    }
    out.push_str(if p.has_cancel {
        "cancel esc\n"
    } else {
        "cancel none\n"
    });
    out
}

/// The compact result `supervise` prints: the phase lines, then the prompt box
/// verbatim, or the last 28 non-blank rows.
pub fn render_result(turn: &Turn, python_allow: &[String]) -> String {
    let mut out = render_phase(turn, python_allow);
    out.push_str("--\n");
    let rows = &turn.screen.rows;
    let span = (turn.phase == Phase::Prompt)
        .then(|| prompt_box_span(rows))
        .flatten();
    match span {
        Some((a, b)) => {
            for r in &rows[a..=b] {
                out.push_str(r);
                out.push('\n');
            }
        }
        None => {
            let kept: Vec<&String> = rows.iter().filter(|r| !r.trim().is_empty()).collect();
            let start = kept.len().saturating_sub(RESULT_ROWS);
            for r in &kept[start..] {
                out.push_str(r);
                out.push('\n');
            }
        }
    }
    out
}

/// Append `<utc stamp> <line>` to the notes file, if one was given.
fn append_note(path: Option<&Path>, line: &str) -> Result<(), String> {
    let Some(path) = path else {
        return Ok(());
    };
    use std::io::Write as _;
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| format!("cannot open notes file {}: {e}", path.display()))?;
    writeln!(f, "{} {line}", utc_stamp(secs))
        .map_err(|e| format!("cannot append to notes file {}: {e}", path.display()))
}

/// `YYYY-MM-DDTHH:MM:SSZ` for a Unix time (civil-from-days, Howard Hinnant).
pub fn utc_stamp(secs: u64) -> String {
    let days = i64::try_from(secs / 86_400).unwrap_or(0);
    let rem = secs % 86_400;
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mo <= 2 { y + 1 } else { y };
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

#[cfg(test)]
mod tests {
    use super::super::prompt::fixtures::{bash_one_row, composer, rows};
    use super::*;
    use std::collections::VecDeque;

    /// A scripted server: each `call` pops the next reply and records the
    /// request; screens are served in order from `screens`.
    ///
    /// The model behind the built-in answers: the content sequence advances on
    /// every read; `await seq <n>` latches only while another screen is still
    /// to come (the worker moved on) and TIMES OUT on the last one (the screen
    /// sits unchanged); a modern `key if=` is judged against the screen last
    /// served — `OK seq=<n>` when a row has `Do you want to proceed`, else `OK
    /// skipped seq=<n>` — and an older host answers it with the bare `ERR`
    /// measured on aterm 0.81.0.
    struct Mock {
        requests: Vec<String>,
        /// Replies to non-`text`, non-`key` requests, in order (missing = the
        /// built-in answer).
        replies: VecDeque<CtlReply>,
        /// Scripted replies to `key` requests, in order (missing = the model).
        key_replies: VecDeque<CtlReply>,
        /// Screens to serve to `text` requests, in order (the last repeats).
        screens: Vec<Vec<String>>,
        served: usize,
        /// Whether the server knows `tail=`, `await gone`, `key if=`.
        modern: bool,
        seq: u64,
    }

    fn ok(stdout: &str) -> CtlReply {
        CtlReply {
            code: 0,
            stdout: stdout.to_string(),
            stderr: String::new(),
        }
    }
    fn timeout() -> CtlReply {
        CtlReply {
            code: 124,
            stdout: "OK timeout\n".to_string(),
            stderr: String::new(),
        }
    }
    fn usage(verb: &str) -> CtlReply {
        err(&format!("usage: {verb}"))
    }
    fn err(text: &str) -> CtlReply {
        CtlReply {
            code: 1,
            stdout: String::new(),
            stderr: format!("aterm-ctl: ERR {text}\n"),
        }
    }
    /// What aterm 0.81.0 answers `key if=… 1` (and `key nosuchkey`): the
    /// option read as a key name, `aterm-ctl: ERR` and nothing more.
    fn bare_err() -> CtlReply {
        CtlReply {
            code: 1,
            stdout: String::new(),
            stderr: "aterm-ctl: ERR\n".to_string(),
        }
    }

    impl Mock {
        fn new(modern: bool, screens: Vec<Vec<String>>) -> Self {
            Self {
                requests: Vec::new(),
                replies: VecDeque::new(),
                key_replies: VecDeque::new(),
                screens,
                served: 0,
                modern,
                seq: 100,
            }
        }
        fn last_served(&self) -> &[String] {
            &self.screens[self.served.saturating_sub(1).min(self.screens.len() - 1)]
        }
        fn screen_json(&mut self) -> String {
            let i = self.served.min(self.screens.len() - 1);
            self.served += 1;
            self.seq += 1;
            let rows: Vec<String> = self.screens[i]
                .iter()
                .map(|r| format!("\"{}\"", r.replace('\\', "\\\\").replace('"', "\\\"")))
                .collect();
            let composer_col = if self.screens[i].iter().any(|r| r == "❯ 1") {
                3
            } else {
                2
            };
            format!(
                "{{\"rows\":[{}],\"cursor\":{{\"row\":10,\"col\":{composer_col},\"visible\":true,\"style\":\"block\"}},\"dims\":{{\"rows\":40,\"cols\":120}},\"seq\":{}}}\n",
                rows.join(","),
                self.seq
            )
        }
        fn presses(&self) -> Vec<&String> {
            self.requests
                .iter()
                .filter(|r| r.contains("key "))
                .collect()
        }
    }

    impl Ctl for Mock {
        fn call(&mut self, args: &[&str]) -> Result<CtlReply, String> {
            let line = args.join(" ");
            self.requests.push(line.clone());
            let verb_at = usize::from(args.first().is_some_and(|a| a.starts_with('@')));
            let verb = args.get(verb_at).copied().unwrap_or("");
            let tail = &args[verb_at + 1..];
            match verb {
                "text" => {
                    if tail.iter().any(|a| a.starts_with("tail=")) && !self.modern {
                        return Ok(usage("text [--json] [trim]"));
                    }
                    let body = self.screen_json();
                    Ok(ok(&body))
                }
                "await" if tail.first() == Some(&"gone") && !self.modern => {
                    Ok(usage("await <idle|seq|match|block>"))
                }
                "await" if tail.first() == Some(&"seq") => {
                    if let Some(r) = self.replies.pop_front() {
                        return Ok(r);
                    }
                    Ok(if self.served < self.screens.len() {
                        ok("OK seq\n")
                    } else {
                        timeout()
                    })
                }
                "key" => {
                    if let Some(r) = self.key_replies.pop_front() {
                        return Ok(r);
                    }
                    let guarded = tail.first().is_some_and(|a| a.starts_with("if="));
                    if guarded && !self.modern {
                        return Ok(bare_err());
                    }
                    let seq = self.seq;
                    if guarded
                        && !self
                            .last_served()
                            .iter()
                            .any(|r| r.contains("Do you want to proceed"))
                    {
                        return Ok(ok(&format!("OK skipped seq={seq}\n")));
                    }
                    Ok(ok(&format!("OK seq={seq}\n")))
                }
                _ => Ok(self.replies.pop_front().unwrap_or_else(|| ok("OK\n"))),
            }
        }
    }

    fn busy_screen() -> Vec<String> {
        let mut r = rows(&["⏺ Working.", "", "✻ Synthesizing… (18s)", ""]);
        r.extend(composer("  esc to interrupt"));
        r
    }
    fn idle_screen() -> Vec<String> {
        let mut r = rows(&["⏺ Done.", "", "✻ Cogitated for 4s · done 2:41 PM", ""]);
        r.extend(composer("  ? for shortcuts"));
        r
    }
    fn write_prompt() -> Vec<String> {
        let mut r = bash_one_row();
        r[4] = "   rm -rf target".to_string();
        r
    }
    fn notes_file(tag: &str) -> (PathBuf, PathBuf) {
        let dir =
            std::env::temp_dir().join(format!("aterm-supervise-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("tmp dir");
        let notes = dir.join("notes.txt");
        let _ = std::fs::remove_file(&notes);
        (dir, notes)
    }
    fn read_notes(dir: &Path, notes: &Path) -> Vec<String> {
        let text = std::fs::read_to_string(notes).unwrap_or_default();
        let lines = text.lines().map(str::to_string).collect();
        let _ = std::fs::remove_file(notes);
        let _ = std::fs::remove_dir(dir);
        lines
    }
    fn auto(max_s: u64, notes: Option<PathBuf>) -> SuperviseOpts {
        SuperviseOpts {
            auto_reads: true,
            max: Duration::from_secs(max_s),
            python_allow: vec![],
            notes,
        }
    }

    /// Modern host: the first wait is `await gone`, reads use `tail=40`, and a
    /// busy read is followed by `await seq <seq>` before the next idle wait.
    #[test]
    fn await_turn_uses_gone_then_idle_seq_on_a_modern_host() {
        let mut m = Mock::new(true, vec![busy_screen(), idle_screen()]);
        let mut s = Session::new(&mut m, Some("@s-1".to_string()));
        let t = s.await_turn(Duration::from_secs(30)).expect("turn");
        assert_eq!(t.phase, Phase::Idle);
        assert!(!t.timed_out);
        assert_eq!(s.caps().gone, Some(true));
        assert_eq!(s.caps().tail, Some(true));
        assert_eq!(
            m.requests,
            [
                "@s-1 await gone esc.to.interrupt timeout 20000",
                "@s-1 text --json tail=40",
                "@s-1 await seq 101 timeout 20000",
                "@s-1 await idle 2000 timeout 20000",
                "@s-1 text --json tail=40",
            ]
        );
    }

    /// Older host: `gone` and `tail=` answer `ERR usage` ONCE each, the loop
    /// falls back to idle and the full read, and never asks again.
    #[test]
    fn await_turn_probes_once_and_falls_back_on_an_older_host() {
        let mut m = Mock::new(false, vec![busy_screen(), idle_screen()]);
        let mut s = Session::new(&mut m, None);
        let t = s.await_turn(Duration::from_secs(30)).expect("turn");
        assert_eq!(t.phase, Phase::Idle);
        assert_eq!(s.caps().gone, Some(false));
        assert_eq!(s.caps().tail, Some(false));
        assert_eq!(
            m.requests,
            [
                "await gone esc.to.interrupt timeout 20000",
                "await idle 2000 timeout 20000",
                "text --json tail=40",
                "text --json",
                "await seq 101 timeout 20000",
                "await idle 2000 timeout 20000",
                "text --json",
            ]
        );
    }

    /// A spent budget with the worker still busy is a timed-out turn, and
    /// `supervise` reports it as TIMEOUT / 124.
    #[test]
    fn a_spent_budget_is_timeout_124() {
        let mut m = Mock::new(true, vec![busy_screen()]);
        let mut s = Session::new(&mut m, None);
        let t = s.await_turn(Duration::ZERO).expect("turn");
        assert_eq!(t.phase, Phase::Busy);
        assert!(t.timed_out);

        let mut m = Mock::new(true, vec![busy_screen()]);
        let mut s = Session::new(&mut m, None);
        let (out, code) = s
            .supervise(&SuperviseOpts {
                max: Duration::ZERO,
                ..SuperviseOpts::default()
            })
            .expect("supervise");
        assert_eq!(code, EXIT_TIMEOUT);
        assert!(out.starts_with("TIMEOUT\nbusy\n--\n"), "{out}");
        assert!(out.contains("✻ Synthesizing"), "{out}");
    }

    /// `--auto-reads`: a read-only Bash prompt is approved with the guarded
    /// press, noted, the loop waits for the box to LEAVE (`await seq`) and
    /// continues with the settle wait, not `await gone`; the next (write)
    /// prompt is handed to the manager with the box printed.
    #[test]
    fn supervise_auto_approves_reads_guarded_and_hands_over_writes() {
        let (dir, notes) = notes_file("approve");
        let mut m = Mock::new(true, vec![bash_one_row(), busy_screen(), write_prompt()]);
        let mut s = Session::new(&mut m, Some("@s-9".to_string()));
        let (out, code) = s
            .supervise(&auto(30, Some(notes.clone())))
            .expect("supervise");
        assert_eq!(code, 0);
        assert!(
            out.starts_with("prompt\nkind bash\ncommand rm -rf target\n"),
            "{out}"
        );
        assert!(out.contains("classify not-read-only rm\n"), "{out}");
        assert!(out.contains("option 1 Yes\n"), "{out}");
        assert!(out.contains("option 4 No\n"), "{out}");
        assert!(
            out.contains("--\n Bash command\n"),
            "the box is printed: {out}"
        );
        assert!(out.ends_with(" Esc to cancel · Tab to amend\n"), "{out}");
        assert_eq!(s.caps().key_if, Some(true));
        assert_eq!(
            m.requests,
            [
                "@s-9 await gone esc.to.interrupt timeout 20000",
                "@s-9 text --json tail=40",
                "@s-9 key if=Do.you.want.to.proceed 1",
                "@s-9 await seq 101 timeout 20000",
                "@s-9 await idle 2000 timeout 20000",
                "@s-9 text --json tail=40",
                "@s-9 await seq 102 timeout 20000",
                "@s-9 await idle 2000 timeout 20000",
                "@s-9 text --json tail=40",
            ],
            "one guarded press, then the box must leave before the next look; \
             after it the settle wait, not `await gone` (the footer is not up yet)"
        );
        let lines = read_notes(&dir, &notes);
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(
            lines[0].ends_with("Z approved read-only: git log --oneline -5"),
            "{lines:?}"
        );
        assert!(
            lines[1].ends_with("Z handed to the manager (rm): rm -rf target"),
            "{lines:?}"
        );
        assert!(lines[0].starts_with("20"), "utc stamp: {lines:?}");
    }

    /// The host this was measured on (aterm 0.81.0) answers `key if=` with a
    /// bare `ERR`, not a usage line: that is "no such form", so the press falls
    /// back — read → confirm → `key 1` → wait for the worker to take it →
    /// re-read — and a `1` that landed in the composer after the box resolved
    /// is backspaced and NOT counted as an approval.
    #[test]
    fn a_bare_err_to_the_guard_falls_back_and_a_stray_digit_is_not_an_approval() {
        let mut stray = idle_screen();
        let c = stray.iter().position(|r| r == "❯").expect("composer");
        stray[c] = "❯ 1".to_string();
        // Reads: the turn's read (prompt), the confirm read (prompt still up),
        // the re-read (resolved, digit in the composer), then idle.
        let (dir, notes) = notes_file("stray");
        let mut m = Mock::new(
            false,
            vec![bash_one_row(), bash_one_row(), stray, idle_screen()],
        );
        let mut s = Session::new(&mut m, None);
        let (out, code) = s
            .supervise(&auto(30, Some(notes.clone())))
            .expect("supervise");
        assert_eq!(code, 0);
        assert!(out.starts_with("idle\n--\n"), "{out}");
        assert_eq!(s.caps().key_if, Some(false), "a bare ERR is no such form");
        assert_eq!(
            m.requests,
            [
                "await gone esc.to.interrupt timeout 20000",
                "await idle 2000 timeout 20000",
                "text --json tail=40",
                "text --json",
                "key if=Do.you.want.to.proceed 1",
                "text --json",
                "key 1",
                "await seq 102 timeout 2000",
                "text --json",
                "key backspace",
                "await idle 2000 timeout 20000",
                "text --json",
            ],
            "{:?}",
            m.requests
        );
        let lines = read_notes(&dir, &notes);
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(
            lines[0]
                .ends_with("nothing pressed, the box had left the screen: git log --oneline -5"),
            "no approval was made: {lines:?}"
        );
    }

    /// Older host, the box still up at the confirm read: the fallback press
    /// answers it, and the loop waits for the box to leave as on a modern host.
    #[test]
    fn the_fallback_press_approves_a_box_that_is_still_up() {
        let (dir, notes) = notes_file("fallback");
        let mut m = Mock::new(
            false,
            vec![bash_one_row(), bash_one_row(), busy_screen(), idle_screen()],
        );
        let mut s = Session::new(&mut m, None);
        let (out, code) = s
            .supervise(&auto(30, Some(notes.clone())))
            .expect("supervise");
        assert_eq!(code, 0);
        assert!(out.starts_with("idle\n--\n"), "{out}");
        assert_eq!(
            m.presses(),
            ["key if=Do.you.want.to.proceed 1", "key 1"],
            "{:?}",
            m.requests
        );
        let lines = read_notes(&dir, &notes);
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(
            lines[0].ends_with("approved read-only: git log --oneline -5"),
            "{lines:?}"
        );
    }

    /// `OK skipped seq=<n>` is an answer, not an approval: nothing was
    /// pressed, so nothing is approved or noted as such — the loop looks again.
    #[test]
    fn a_skipped_guard_is_not_an_approval() {
        let (dir, notes) = notes_file("skipped");
        let mut m = Mock::new(true, vec![bash_one_row(), idle_screen()]);
        // The box resolved between the read (seq 101) and the check (seq 105).
        m.key_replies.push_back(ok("OK skipped seq=105\n"));
        let mut s = Session::new(&mut m, None);
        let (out, code) = s
            .supervise(&auto(30, Some(notes.clone())))
            .expect("supervise");
        assert_eq!(code, 0);
        assert!(out.starts_with("idle\n--\n"), "{out}");
        assert_eq!(s.caps().key_if, Some(true), "a skip still proves the guard");
        assert_eq!(m.presses(), ["key if=Do.you.want.to.proceed 1"]);
        let lines = read_notes(&dir, &notes);
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(
            lines[0]
                .ends_with("nothing pressed, the box had left the screen: git log --oneline -5"),
            "{lines:?}"
        );
        assert!(!lines.iter().any(|l| l.contains("approved")), "{lines:?}");
    }

    /// A guard that misses on the VERY screen that was parsed (a Bash box with
    /// no `Do you want to proceed?` row) is the manager's, not a loop.
    #[test]
    fn a_guard_that_matches_no_row_of_the_parsed_box_is_handed_over() {
        let mut box_without_question = bash_one_row();
        box_without_question.retain(|r| !r.contains("Do you want to proceed?"));
        assert_eq!(
            parse_prompt(&box_without_question).map(|p| p.kind),
            Some(PromptKind::Bash)
        );
        let (dir, notes) = notes_file("norow");
        let mut m = Mock::new(true, vec![box_without_question]);
        let mut s = Session::new(&mut m, None);
        let (out, code) = s
            .supervise(&auto(30, Some(notes.clone())))
            .expect("supervise");
        assert_eq!(code, 0);
        assert!(out.starts_with("prompt\nkind bash\n"), "{out}");
        assert_eq!(m.presses(), ["key if=Do.you.want.to.proceed 1"]);
        let lines = read_notes(&dir, &notes);
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(
            lines[0].contains("handed to the manager (the guarded press matched no row)"),
            "{lines:?}"
        );
    }

    /// The other `ERR`s a modern host can answer the guard with are never an
    /// approval: `busy sink` is retried and then pressed; `halted` and `no such
    /// session` surface as errors with nothing approved.
    #[test]
    fn guard_errors_are_retried_or_surfaced_never_approved() {
        let (dir, notes) = notes_file("busy");
        let mut m = Mock::new(true, vec![bash_one_row(), busy_screen(), idle_screen()]);
        m.key_replies.push_back(err("busy sink"));
        let mut s = Session::new(&mut m, None);
        let (out, code) = s
            .supervise(&auto(30, Some(notes.clone())))
            .expect("supervise");
        assert_eq!(code, 0);
        assert!(out.starts_with("idle\n--\n"), "{out}");
        assert_eq!(
            m.presses(),
            [
                "key if=Do.you.want.to.proceed 1",
                "key if=Do.you.want.to.proceed 1"
            ],
            "retried once, then the model pressed"
        );
        let lines = read_notes(&dir, &notes);
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(lines[0].contains("approved read-only"), "{lines:?}");

        for what in ["halted", "no such session", "badregex"] {
            let (dir, notes) = notes_file("halted");
            let mut m = Mock::new(true, vec![bash_one_row()]);
            m.key_replies.push_back(err(what));
            let mut s = Session::new(&mut m, None);
            let e = s
                .supervise(&auto(30, Some(notes.clone())))
                .expect_err("surfaces");
            assert!(e.contains(&format!("ERR {what}")), "{what}: {e}");
            assert_eq!(m.presses().len(), 1, "{what}: {:?}", m.requests);
            let lines = read_notes(&dir, &notes);
            assert!(lines.is_empty(), "{what}: nothing approved: {lines:?}");
        }

        // busy sink that never clears is an error too.
        let mut m = Mock::new(true, vec![bash_one_row()]);
        for _ in 0..=BUSY_SINK_RETRIES {
            m.key_replies.push_back(err("busy sink"));
        }
        let mut s = Session::new(&mut m, None);
        let e = s.supervise(&auto(30, None)).expect_err("surfaces");
        assert!(e.contains("ERR busy sink"), "{e}");
        assert_eq!(m.presses().len(), 1 + BUSY_SINK_RETRIES as usize);
    }

    /// Without `--auto-reads` even a read-only prompt is the manager's; a
    /// question and a non-Bash prompt are too.
    #[test]
    fn without_auto_reads_every_prompt_is_handed_over() {
        let mut m = Mock::new(true, vec![bash_one_row()]);
        let mut s = Session::new(&mut m, None);
        let (out, code) = s
            .supervise(&SuperviseOpts {
                max: Duration::from_secs(5),
                ..SuperviseOpts::default()
            })
            .expect("supervise");
        assert_eq!(code, 0);
        assert!(out.starts_with("prompt\nkind bash\ncommand git log --oneline -5\ndescription Show the five most recent commits\nclassify read-only\n"), "{out}");
        assert!(!m.requests.iter().any(|r| r.starts_with("key")));

        let mut q = rows(&["⏺ Keep the harness or rewrite it?", ""]);
        q.extend(composer("  ? for shortcuts"));
        let mut m = Mock::new(true, vec![q]);
        let mut s = Session::new(&mut m, None);
        let (out, _) = s.supervise(&auto(5, None)).expect("supervise");
        assert!(
            out.starts_with("question\n--\n⏺ Keep the harness or rewrite it?\n"),
            "{out}"
        );
    }

    /// The defect 4268efb74 closed, from the supervisor's side: after a press
    /// the screen is NOT re-read at once. `await gone` is level-triggered and
    /// a box shows no busy footer, so an immediate read of an unchanged screen
    /// would match the same box and press a second `1` — into the composer.
    /// The loop waits for the content to move past the press; a screen that
    /// sits unchanged is pressed ONCE and handed over.
    #[test]
    fn the_same_box_is_never_pressed_twice_on_an_unchanged_screen() {
        let (dir, notes) = notes_file("unchanged");
        let mut m = Mock::new(true, vec![bash_one_row()]);
        let mut s = Session::new(&mut m, None);
        let (out, code) = s
            .supervise(&auto(30, Some(notes.clone())))
            .expect("supervise");
        assert_eq!(code, 0);
        assert!(out.starts_with("prompt\n"), "{out}");
        assert_eq!(
            m.requests,
            [
                "await gone esc.to.interrupt timeout 20000",
                "text --json tail=40",
                "key if=Do.you.want.to.proceed 1",
                "await seq 101 timeout 20000",
                "text --json tail=40",
            ],
            "{:?}",
            m.requests
        );
        let lines = read_notes(&dir, &notes);
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(lines[0].contains("approved read-only"), "{lines:?}");
        assert!(
            lines[1].contains("handed to the manager (the box did not change after the press)"),
            "{lines:?}"
        );
    }

    /// The same read coming back after REAL turns (the screen moved on in
    /// between) is approved twice; the third time it is handed over rather
    /// than pressed forever.
    #[test]
    fn a_read_that_keeps_coming_back_is_approved_twice_then_handed_over() {
        let (dir, notes) = notes_file("repeat");
        let mut m = Mock::new(
            true,
            vec![
                bash_one_row(),
                busy_screen(),
                bash_one_row(),
                busy_screen(),
                bash_one_row(),
            ],
        );
        let mut s = Session::new(&mut m, None);
        let (out, code) = s
            .supervise(&auto(30, Some(notes.clone())))
            .expect("supervise");
        assert_eq!(code, 0);
        assert!(
            out.starts_with("prompt\nkind bash\ncommand git log --oneline -5\n"),
            "{out}"
        );
        assert_eq!(m.presses().len(), 2, "{:?}", m.requests);
        let lines = read_notes(&dir, &notes);
        assert_eq!(lines.len(), 3, "{lines:?}");
        assert!(
            lines[2]
                .contains("handed to the manager (the same prompt came back after two approvals)"),
            "{lines:?}"
        );
    }

    /// An `await` that times out (exit 124) is a normal step of the loop, not
    /// an error: the read that follows decides.
    #[test]
    fn an_await_timeout_is_a_step_not_an_error() {
        let mut m = Mock::new(true, vec![idle_screen()]);
        m.replies.push_back(timeout());
        let mut s = Session::new(&mut m, None);
        let t = s.await_turn(Duration::from_secs(30)).expect("turn");
        assert_eq!(t.phase, Phase::Idle);
        assert_eq!(
            s.caps().gone,
            Some(true),
            "a timeout still proves the verb exists"
        );
        assert_eq!(
            m.requests,
            [
                "await gone esc.to.interrupt timeout 20000",
                "text --json tail=40"
            ]
        );
    }

    #[test]
    fn a_hard_ctl_error_surfaces() {
        let mut m = Mock::new(true, vec![idle_screen()]);
        m.replies.push_back(err("no such session"));
        let mut s = Session::new(&mut m, Some("@nope".to_string()));
        let err = s.await_turn(Duration::from_secs(1)).expect_err("surfaces");
        assert!(err.contains("ERR no such session"), "{err}");
    }

    /// The reply readers the guard depends on, against the measured wire.
    #[test]
    fn reply_readers_match_the_wire() {
        assert_eq!(ok("OK seq=31\n").seq(), Some(31));
        assert_eq!(ok("OK skipped seq=7\n").seq(), Some(7));
        assert!(ok("OK skipped seq=7\n").skipped());
        assert!(!ok("OK seq=7\n").skipped());
        assert_eq!(ok("OK\n").seq(), None);
        assert!(bare_err().bare_err());
        assert!(bare_err().unknown_form());
        assert!(usage("key <name>").unknown_form());
        assert!(!err("busy sink").unknown_form());
        assert!(err("busy sink").is_err("busy sink"));
        assert!(err("halted").is_err("halted"));
        assert!(!err("halted").is_err("busy sink"));
        assert_eq!(err("no such session").err_text(), "ERR no such session");
        assert!(!ok("OK\n").bare_err());
    }

    #[test]
    fn utc_stamp_is_iso_8601() {
        assert_eq!(utc_stamp(0), "1970-01-01T00:00:00Z");
        assert_eq!(utc_stamp(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(utc_stamp(1_757_527_218), "2025-09-10T18:00:18Z");
    }
}
