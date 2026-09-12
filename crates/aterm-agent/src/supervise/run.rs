// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The supervisor loop over the control verbs: wait for the worker's turn to
//! end ([`Session::await_turn`]), then either approve a read-only Bash prompt
//! itself or hand the screen to the manager — once, and exit
//! ([`Session::supervise`]), or as one line per decision while it keeps
//! watching ([`Session::watch`]). Every request goes through one [`Ctl`] seam
//! — the `aterm-ctl` launcher the other drive subcommands use in production, a
//! scripted mock in the tests — and the server features newer builds add
//! (`text … tail=`, `await gone`, `key if=`) are probed ONCE and remembered, so
//! the loop runs against an older host too.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::classify::{DEFAULT_PYTHON_ALLOW, Verdict, classify_command_with};
use super::phase::{
    Phase, busy_signal, composer_text, has_composer_frame, is_placeholder, last_said_index,
    last_said_row, status_row, worker_phase,
};
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

/// `supervise`'s knobs (and `watch`'s: the same loop).
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
/// How many characters of a command, a said row or a notice one `watch` line
/// carries.
const LINE_CHARS: usize = 160;
/// How many non-blank rows of the transcript, up to the last thing said (or
/// up to a prompt box), make a review point's words ([`review_key`]).
const KEY_ROWS: usize = 8;

/// The exit code `supervise`, `watch` and `await-turn` end with on a spent
/// budget.
pub const EXIT_TIMEOUT: u8 = 124;

enum Wait {
    Latched,
    TimedOut,
    Unsupported,
}

/// How the shared loop ended.
enum End {
    /// The review stopped the loop at this point (`supervise`).
    Stopped(Turn),
    /// The budget ran out; the last screen read.
    Timeout(Turn),
}

/// What [`Session::auto_read`] made of a turn.
enum Step {
    /// A read was approved, or the box had left: look again. `settle` makes
    /// the first wait the settle wait, not `await gone` (the busy footer is not
    /// up yet right after a press).
    Again { settle: bool },
    /// A review point: the manager's.
    Review(Turn),
}

/// The half of the loop that differs between `supervise` and `watch`.
trait Review {
    /// A read-only prompt was approved; the press landed at `seq`.
    fn approved(&mut self, seq: u64, command: &str) -> Result<(), String>;
    /// A new review point. `false` stops the loop with it (`supervise`);
    /// `true` means it was reported and the loop keeps watching (`watch`).
    fn review(&mut self, turn: &Turn) -> Result<bool, String>;
}

/// `supervise`: the first review point ends the loop, and is its result.
struct StopAtReview;

impl Review for StopAtReview {
    fn approved(&mut self, _seq: u64, _command: &str) -> Result<(), String> {
        Ok(())
    }
    fn review(&mut self, _turn: &Turn) -> Result<bool, String> {
        Ok(false)
    }
}

/// `watch`: one flushed line per approval and per review point.
struct Lines<'w> {
    out: &'w mut dyn Write,
    allow: &'w [String],
}

impl Review for Lines<'_> {
    fn approved(&mut self, seq: u64, command: &str) -> Result<(), String> {
        emit(self.out, &format!("APPROVED seq={seq} {}", clip(command)))
    }
    fn review(&mut self, turn: &Turn) -> Result<bool, String> {
        emit(self.out, &event_line(turn, self.allow))?;
        Ok(true)
    }
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
    /// an older build. A screen without Claude Code's composer frame (a build,
    /// a script, a REPL) that never held still for the 2 s and shows no
    /// approval box is output still arriving, and counts as busy: its turn
    /// ends when the output pauses.
    pub fn await_turn(&mut self, timeout: Duration) -> Result<Turn, String> {
        self.await_turn_from(timeout, true).map(|(turn, _)| turn)
    }

    /// [`Self::await_turn`], with the `await gone` first wait optional: right
    /// after an approval the busy footer is not up YET, so its leaving is no
    /// signal — the settle wait is the first one. Also says whether a read on
    /// the way was busy: the worker moved since the last look.
    fn await_turn_from(
        &mut self,
        timeout: Duration,
        gone_first: bool,
    ) -> Result<(Turn, bool), String> {
        let deadline = Instant::now() + timeout;
        let mut first = gone_first;
        let mut saw_busy = false;
        loop {
            let step = deadline
                .saturating_duration_since(Instant::now())
                .min(WAIT_STEP);
            // The first wait was the busy footer leaving, which says nothing
            // about the screen holding still.
            let mut gone = false;
            // The screen held still for IDLE_MS right before the read.
            let mut settled = false;
            if first && self.caps.gone != Some(false) {
                match self.wait(&["gone", BUSY_FOOTER], step)? {
                    Wait::Unsupported => self.caps.gone = Some(false),
                    _ => {
                        self.caps.gone = Some(true);
                        gone = true;
                    }
                }
            }
            if !gone {
                settled = matches!(self.wait(&["idle", IDLE_MS], step)?, Wait::Latched);
            }
            first = false;
            let screen = self.read_screen()?;
            let mut phase = worker_phase(&screen.rows);
            // No composer frame and the screen never held still: a build's or a
            // REPL's output still arriving, not the end of anything.
            let writing = !settled
                && !matches!(phase, Phase::Busy | Phase::Prompt)
                && !has_composer_frame(&screen.rows);
            if phase != Phase::Busy && !writing {
                let turn = Turn {
                    phase,
                    screen,
                    timed_out: false,
                };
                return Ok((turn, saw_busy));
            }
            if writing {
                phase = Phase::Busy;
            }
            if !(writing && gone) {
                saw_busy = true;
            }
            if Instant::now() >= deadline {
                let turn = Turn {
                    phase,
                    screen,
                    timed_out: true,
                };
                return Ok((turn, saw_busy));
            }
            if writing && gone {
                // Settle first: the next wait is the idle one.
                continue;
            }
            let seq = screen.seq.to_string();
            self.wait(&["seq", &seq], step)?;
        }
    }

    /// The loop. Returns the text to print and the exit code: `0` with the
    /// compact result when the worker needs the manager (a non-read prompt, a
    /// question, a limit notice, an idle composer); [`EXIT_TIMEOUT`] with
    /// `TIMEOUT` and the last read's compact result when the budget is spent —
    /// a turn read at or after the deadline is the TIMEOUT, never pressed.
    pub fn supervise(&mut self, opts: &SuperviseOpts) -> Result<(String, u8), String> {
        let allow = opts.allow();
        Ok(match self.drive(opts, &mut StopAtReview)? {
            End::Stopped(turn) => (render_result(&turn, &allow), 0),
            End::Timeout(turn) => (
                format!("TIMEOUT\n{}", render_result(&turn, &allow)),
                EXIT_TIMEOUT,
            ),
        })
    }

    /// `supervise`'s loop for a harness that wakes its agent once per stdout
    /// line (a background monitor, a supervisor process). A review point prints
    /// ONE line, [`event_line`], and the loop keeps watching: it waits for the
    /// screen to move past that point (`await seq`) before it looks again, so a
    /// manager's turn or key is picked up without a relaunch. A point that
    /// looks the same as the one last reported ([`review_key`]: the same phase,
    /// summary and status row, the same last rows of the transcript up to it,
    /// the same box) is neither pressed nor printed again unless a read in
    /// between saw the worker busy or the loop approved a read — so a footer
    /// that ticks does not repeat an EVENT, while a new box, a new reply, a
    /// manager's row or a retry's new notice does, however short the busy
    /// spell before it. An approval prints `APPROVED seq=<n> <command>`. The
    /// last line is `TIMEOUT` (returns [`EXIT_TIMEOUT`]) when the budget is
    /// spent — no press and no EVENT comes after the deadline — or `EXIT
    /// <reason>` (returns 1) when the session goes or the loop fails (a
    /// request, the notes file). Every line is flushed as it is written.
    pub fn watch(&mut self, opts: &SuperviseOpts, out: &mut dyn Write) -> u8 {
        let allow = opts.allow();
        let end = self.drive(
            opts,
            &mut Lines {
                out: &mut *out,
                allow: &allow,
            },
        );
        let (line, code) = match end {
            Ok(End::Timeout(_)) => ("TIMEOUT".to_string(), EXIT_TIMEOUT),
            // `Lines` never stops at a review point; kept for the match.
            Ok(End::Stopped(_)) => ("EXIT stopped at a review point".to_string(), 1),
            Err(e) => (format!("EXIT {}", exit_reason(&e)), 1),
        };
        // Nothing is left to report a failed write to.
        let _ = emit(out, &line);
        code
    }

    /// The loop `supervise` and `watch` share: await the turn; with
    /// `--auto-reads`, approve a read-only Bash prompt ([`Self::auto_read`]);
    /// anything else is a review point for `review`, which stops the loop or
    /// reports the point and keeps watching — and then the loop waits for the
    /// screen to move past it before the next look.
    fn drive(&mut self, opts: &SuperviseOpts, review: &mut dyn Review) -> Result<End, String> {
        let deadline = Instant::now() + opts.max;
        let allow = opts.allow();
        let mut approved: Vec<String> = Vec::new();
        // The first wait of the next turn: the busy footer leaving, except
        // right after a press or a review point, when it is not up yet.
        let mut gone_first = true;
        // The review point last handed over, until the worker moves.
        let mut handed: Option<String> = None;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let (turn, moved) = self.await_turn_from(remaining, gone_first)?;
            if moved {
                handed = None;
            }
            // The budget bounds every action: a turn read at or after the
            // deadline is neither pressed nor reported — it is the TIMEOUT.
            if turn.timed_out || Instant::now() >= deadline {
                return Ok(End::Timeout(turn));
            }
            let seen = if handed.as_deref() == Some(review_key(&turn, &allow).as_str()) {
                // The point already handed over, still showing: the screen
                // moved (a footer tick, a banner) and nothing the point is made
                // of did. Nothing is pressed or reported.
                turn
            } else {
                let point =
                    match self.auto_read(turn, opts, &allow, &mut approved, deadline, review)? {
                        Step::Again { settle } => {
                            handed = None;
                            gone_first = !settle;
                            continue;
                        }
                        Step::Review(point) => point,
                    };
                if !review.review(&point)? {
                    return Ok(End::Stopped(point));
                }
                // The manager has the worker now, as after a fresh `supervise`.
                approved.clear();
                handed = Some(review_key(&point, &allow));
                point
            };
            if !self.wait_past(seen.screen.seq, deadline)? {
                return Ok(End::Timeout(seen));
            }
            gone_first = false;
        }
    }

    /// `--auto-reads` on one turn: a Bash prompt whose command classifies
    /// read-only is approved with the guarded press (noted, and reported to
    /// `review`), and the loop looks again once the box has LEFT; anything
    /// else — not a prompt, not Bash, not read-only, the same read back after
    /// two approvals, a guard that matched no row of this very box, a box that
    /// did not move after the press — is a review point.
    fn auto_read(
        &mut self,
        turn: Turn,
        opts: &SuperviseOpts,
        allow: &[String],
        approved: &mut Vec<String>,
        deadline: Instant,
        review: &mut dyn Review,
    ) -> Result<Step, String> {
        let bash = (opts.auto_reads && turn.phase == Phase::Prompt)
            .then(|| parse_prompt(&turn.screen.rows))
            .flatten()
            .filter(|p| p.kind == PromptKind::Bash);
        let Some(p) = bash else {
            return Ok(Step::Review(turn));
        };
        let v = classify_command_with(&p.command, allow);
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
            return Ok(Step::Review(turn));
        }
        match self.press_one_guarded(&p, &turn.screen)? {
            Press::Pressed { seq } => {
                approved.push(p.command.clone());
                append_note(
                    opts.notes.as_deref(),
                    &format!("approved read-only: {}", p.command),
                )?;
                review.approved(seq, &p.command)?;
                // The box must LEAVE before the next look: `await gone` is
                // level-triggered and a box shows no busy footer, so an
                // immediate re-read of an unchanged screen would match the same
                // box and press it again — the stray digit the guard exists to
                // prevent.
                if !self.moved_past(seq, deadline)? {
                    append_note(
                        opts.notes.as_deref(),
                        &format!(
                            "handed to the manager (the box did not change after the press): {}",
                            p.command
                        ),
                    )?;
                    return Ok(Step::Review(self.current_turn()?));
                }
                Ok(Step::Again { settle: true })
            }
            Press::Skipped { seq } => {
                if seq == turn.screen.seq {
                    // The very screen we parsed, and the guard found no row:
                    // this box is not one the supervisor answers.
                    append_note(
                        opts.notes.as_deref(),
                        &format!(
                            "handed to the manager (the guarded press matched no row): {}",
                            p.command
                        ),
                    )?;
                    return Ok(Step::Review(turn));
                }
                append_note(
                    opts.notes.as_deref(),
                    &format!(
                        "nothing pressed, the box had left the screen: {}",
                        p.command
                    ),
                )?;
                Ok(Step::Again { settle: false })
            }
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

    /// Wait for the content to move past `seq`, step after step, until
    /// `deadline`: `false` when the budget ran out with the screen unchanged.
    fn wait_past(&mut self, seq: u64, deadline: Instant) -> Result<bool, String> {
        while Instant::now() < deadline {
            if self.moved_past(seq, deadline)? {
                return Ok(true);
            }
        }
        Ok(false)
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
/// its parsed kind, command, description, classification and options; for
/// busy, `reason <zone>: <rule>` (which signal fired, and where — `whole
/// screen, no composer frame: output still changing` when a wait ran out on a
/// screen without the composer frame that kept changing); for a limit notice,
/// `message <text>` and `reset <text|->`.
pub fn render_phase(turn: &Turn, python_allow: &[String]) -> String {
    let mut out = format!("{}\n", turn.phase.name());
    match &turn.phase {
        Phase::Prompt => {
            if let Some(p) = parse_prompt(&turn.screen.rows) {
                out.push_str(&render_prompt(&p, python_allow));
            }
        }
        Phase::Busy => match busy_signal(&turn.screen.rows) {
            Some(b) => out.push_str(&format!("reason {b}\n")),
            // Only a timed-out wait says this: output still arriving on a
            // screen without the composer frame (see `await_turn_from`).
            None => out.push_str("reason whole screen, no composer frame: output still changing\n"),
        },
        Phase::Limited { message, reset } => {
            out.push_str(&format!(
                "message {message}\nreset {}\n",
                reset.as_deref().unwrap_or("-")
            ));
        }
        Phase::Idle | Phase::Question => {}
    }
    out
}

/// The one line `watch` prints at a review point: `EVENT <phase> seq=<n>
/// <summary>`, the summary being, for a prompt, `kind=<k> classify=<read-only|
/// not-read-only:<reason>> command=<command>` (`classify=-` for a box that is
/// not Bash; a workflow's description stands in for its command); for a limit
/// notice, `message=<text> reset=<text|->`; otherwise the last row the worker
/// said. Each field is cut at 160 characters.
pub fn event_line(turn: &Turn, python_allow: &[String]) -> String {
    format!(
        "EVENT {} seq={} {}",
        turn.phase.name(),
        turn.screen.seq,
        event_summary(turn, python_allow)
    )
}

fn event_summary(turn: &Turn, python_allow: &[String]) -> String {
    match &turn.phase {
        Phase::Prompt => {
            let Some(p) = parse_prompt(&turn.screen.rows) else {
                return "kind=other classify=- command=-".to_string();
            };
            let classify = if p.kind == PromptKind::Bash {
                let v = classify_command_with(&p.command, python_allow);
                if v.read_only {
                    "read-only".to_string()
                } else {
                    format!("not-read-only:{}", v.reason)
                }
            } else {
                "-".to_string()
            };
            let what = if p.command.is_empty() && p.kind == PromptKind::Workflow {
                p.description.as_str()
            } else {
                p.command.as_str()
            };
            format!(
                "kind={} classify={classify} command={}",
                p.kind.name(),
                or_dash(&clip(what))
            )
        }
        Phase::Limited { message, reset } => format!(
            "message={} reset={}",
            clip(message),
            reset.as_deref().map_or_else(|| "-".to_string(), clip)
        ),
        Phase::Busy | Phase::Idle | Phase::Question => {
            or_dash(&clip(last_said_row(&turn.screen.rows).unwrap_or("").trim()))
        }
    }
}

/// What makes two review points the same one — never the seq, which any
/// footer tick moves: the phase, the event summary, the status row, and the
/// words of the last [`KEY_ROWS`] non-blank rows of the transcript, up to the
/// last thing said or, for a prompt, up to its box, whose rows count verbatim.
/// Anything the worker, a tool or the manager adds lands in those rows (a new
/// box comes after the output of the one before it, a retry after its `❯`
/// row), while a footer or a banner does not; letters only, so a timer that
/// ticks or a glyph that blinks does not make a row new.
fn review_key(turn: &Turn, python_allow: &[String]) -> String {
    let rows = &turn.screen.rows;
    let mut key = format!(
        "{} {}\n{}\n",
        turn.phase.name(),
        event_summary(turn, python_allow),
        status_row(rows).unwrap_or("")
    );
    let span = (turn.phase == Phase::Prompt)
        .then(|| prompt_box_span(rows))
        .flatten();
    let upto = match span {
        Some((a, _)) => a,
        None => last_said_index(rows).map_or(0, |k| k + 1),
    };
    let said: Vec<&String> = rows[..upto]
        .iter()
        .filter(|r| !r.trim().is_empty())
        .collect();
    for r in &said[said.len().saturating_sub(KEY_ROWS)..] {
        key.extend(r.chars().filter(|c| c.is_alphabetic()));
        key.push('\n');
    }
    if let Some((a, b)) = span {
        for r in &rows[a..=b] {
            key.push_str(r.trim_end());
            key.push('\n');
        }
    }
    key
}

/// The first [`LINE_CHARS`] characters of `s`, on one line.
fn clip(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(LINE_CHARS)
        .collect()
}

fn or_dash(s: &str) -> String {
    if s.is_empty() {
        "-".to_string()
    } else {
        s.to_string()
    }
}

/// `watch`'s `EXIT` reason, on one line: the failed request, named as the
/// session going when that is what the host said — `ERR exited` to a wait in
/// progress when the session ends, `ERR no such session` to a request after.
pub fn exit_reason(err: &str) -> String {
    let err: String = err
        .trim()
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    if err.contains("ERR exited") || err.contains("no such session") {
        format!("session gone ({err})")
    } else {
        err
    }
}

/// Write one line and flush it: the harness reading `watch` wakes per line.
fn emit(out: &mut dyn Write, line: &str) -> Result<(), String> {
    writeln!(out, "{line}")
        .and_then(|()| out.flush())
        .map_err(|e| format!("cannot write a line to stdout: {e}"))
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
    use super::super::prompt::fixtures::{bash_one_row, composer, rows, workflow_box};
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
    ///
    /// `await idle` answers at once by default, so every scripted screen is
    /// read. The real server's `await idle 2000` latches only once the content
    /// held still for 2 s, and a turning spinner never does: with
    /// `idle_skips_busy`, an `await idle` passes over the busy screens next in
    /// the script (the content moves, nothing reads them) when another screen
    /// follows them — a busy spell the loop never sees.
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
        /// After this many `await seq` timeouts on the last screen the session
        /// is gone — the wait answers `ERR exited`, as the real server's await
        /// does when the session ends under it — which is how a `watch` test
        /// ends.
        vanish_after: Option<u32>,
        stalls: u32,
        /// `await idle` passes over busy screens (see the struct doc).
        idle_skips_busy: bool,
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
                vanish_after: None,
                stalls: 0,
                idle_skips_busy: false,
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
                    if self.served < self.screens.len() {
                        return Ok(ok("OK seq\n"));
                    }
                    if let Some(n) = self.vanish_after {
                        if self.stalls >= n {
                            return Ok(err("exited"));
                        }
                        self.stalls += 1;
                    }
                    Ok(timeout())
                }
                "await" if tail.first() == Some(&"idle") && self.idle_skips_busy => {
                    if let Some(r) = self.replies.pop_front() {
                        return Ok(r);
                    }
                    while self.served + 1 < self.screens.len()
                        && worker_phase(&self.screens[self.served]) == Phase::Busy
                    {
                        self.served += 1;
                        self.seq += 1;
                    }
                    Ok(ok("OK idle\n"))
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
        assert!(
            out.starts_with("TIMEOUT\nbusy\nreason status row: spinner\n--\n"),
            "{out}"
        );
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

    fn question_screen() -> Vec<String> {
        let mut q = rows(&["⏺ Keep the harness or rewrite it?", ""]);
        q.extend(composer("  ? for shortcuts"));
        q
    }
    fn limited_screen() -> Vec<String> {
        let mut r = rows(&[
            "⏺ Running the suite again.",
            "  ⎿  You've hit your session limit · resets 7:30pm (America/Los_Angeles)",
            "",
            "✻ Worked for 3m 2s · done 5:02 PM",
            "",
        ]);
        r.extend(composer("  ⏵⏵ auto mode on (shift+tab to cycle)"));
        r
    }
    /// `body` over an idle composer frame.
    fn framed(body: &[&str]) -> Vec<String> {
        let mut r = rows(body);
        r.extend(composer("  ? for shortcuts"));
        r
    }
    /// A Bash box for `command` under `transcript`.
    fn bash_box(transcript: &[&str], command: &str) -> Vec<String> {
        let mut r = rows(transcript);
        r.extend(rows(&[
            "",
            " Bash command",
            "",
            &format!("   {command}"),
            "   Push the branch",
            "",
            " Do you want to proceed?",
            " ❯ 1. Yes",
            "   2. Yes, and don’t ask again for: git push *",
            "   3. No",
            "",
            " Esc to cancel · Tab to amend",
        ]));
        r.extend(composer("  ? for shortcuts"));
        r
    }
    /// An Edit box for one file, changing `a` to `value`, under `transcript`.
    fn edit_box(transcript: &[&str], value: &str) -> Vec<String> {
        let mut r = rows(transcript);
        r.extend(rows(&[
            "",
            " Edit file",
            "",
            "   crates/foo/src/lib.rs",
            "",
            "   12    -    let a = 1;",
            &format!("   12    +    let a = {value};"),
            "",
            " Do you want to make this edit to lib.rs?",
            " ❯ 1. Yes",
            "   2. Yes, allow all edits during this session (shift+tab)",
            "   3. No",
            "",
            " Esc to cancel · Tab to amend",
        ]));
        r.extend(composer("  ? for shortcuts"));
        r
    }
    fn busy_over(transcript: &[&str]) -> Vec<String> {
        let mut r = rows(transcript);
        r.extend(rows(&["", "✻ Pushing… (3s)", ""]));
        r.extend(composer("  esc to interrupt"));
        r
    }
    fn watch_lines(m: &mut Mock, opts: &SuperviseOpts) -> (Vec<String>, u8) {
        let mut out: Vec<u8> = Vec::new();
        let code = Session::new(m, None).watch(opts, &mut out);
        let text = String::from_utf8(out).expect("utf-8");
        (text.lines().map(str::to_string).collect(), code)
    }
    /// The EVENT and APPROVED lines, without the seq (which counts reads).
    fn decisions(lines: &[String]) -> Vec<String> {
        lines
            .iter()
            .map(|l| {
                let mut words: Vec<&str> = l.split(' ').collect();
                words.retain(|w| !w.starts_with("seq="));
                words.join(" ")
            })
            .collect()
    }

    /// `phase` says WHY busy is busy, and what a limit notice says.
    #[test]
    fn render_phase_names_the_busy_rule_and_the_limit() {
        let busy = Turn {
            phase: worker_phase(&busy_screen()),
            screen: Screen {
                rows: busy_screen(),
                ..Screen::default()
            },
            timed_out: false,
        };
        assert_eq!(
            render_phase(&busy, &[]),
            "busy\nreason status row: spinner\n"
        );
        let rows = limited_screen();
        let limited = Turn {
            phase: worker_phase(&rows),
            screen: Screen {
                rows,
                ..Screen::default()
            },
            timed_out: false,
        };
        assert_eq!(
            render_phase(&limited, &[]),
            "limited\nmessage You've hit your session limit · resets 7:30pm \
             (America/Los_Angeles)\nreset 7:30pm (America/Los_Angeles)\n"
        );
    }

    /// The EVENT summary is the worker's last row, never the session survey
    /// Claude Code parks under the done row (the saved wait_bg7 screen).
    #[test]
    fn an_event_summary_is_what_the_worker_said_not_the_survey() {
        let mut rows: Vec<String> = include_str!("fixtures/wait_bg7.out")
            .lines()
            .map(str::to_string)
            .collect();
        rows.remove(0);
        rows.pop();
        let turn = Turn {
            phase: worker_phase(&rows),
            screen: Screen {
                rows,
                seq: 7,
                ..Screen::default()
            },
            timed_out: false,
        };
        assert_eq!(
            event_line(&turn, &[]),
            "EVENT idle seq=7 bookkeeping, then site B, then compaction, then the post-merge \
             reschedule. Waiting for your call."
        );
    }

    /// `supervise` hands a limit notice to the manager exactly like idle.
    #[test]
    fn supervise_hands_a_limit_notice_over_like_idle() {
        let mut m = Mock::new(true, vec![busy_screen(), limited_screen()]);
        let mut s = Session::new(&mut m, None);
        let (out, code) = s.supervise(&auto(30, None)).expect("supervise");
        assert_eq!(code, 0);
        assert!(
            out.starts_with(
                "limited\nmessage You've hit your session limit · resets 7:30pm \
                 (America/Los_Angeles)\nreset 7:30pm (America/Los_Angeles)\n--\n"
            ),
            "{out}"
        );
        assert!(m.presses().is_empty());
    }

    /// The watch loop, scripted: busy → a read-only prompt (approved, one
    /// `APPROVED` line) → busy → a question (one `EVENT`) → the manager's turn
    /// (busy) → idle (one `EVENT`) → the idle screen sits unchanged (no second
    /// `EVENT`, however long) → the session ends under the wait (`ERR exited`,
    /// `EXIT session gone`, 1). After each EVENT the loop waits for the screen
    /// to move past it, then settles before the next look.
    #[test]
    fn watch_prints_one_line_per_decision_and_keeps_watching() {
        let (dir, notes) = notes_file("watch");
        let mut m = Mock::new(
            true,
            vec![
                busy_screen(),
                bash_one_row(),
                busy_screen(),
                question_screen(),
                busy_screen(),
                idle_screen(),
            ],
        );
        m.vanish_after = Some(3);
        let (lines, code) = watch_lines(&mut m, &auto(30, Some(notes.clone())));
        assert_eq!(
            lines,
            [
                "APPROVED seq=102 git log --oneline -5",
                "EVENT question seq=104 ⏺ Keep the harness or rewrite it?",
                "EVENT idle seq=106 ⏺ Done.",
                "EXIT session gone (await seq 106 failed: aterm-ctl: ERR exited)",
            ]
        );
        assert_eq!(code, 1);
        assert_eq!(
            m.requests,
            [
                "await gone esc.to.interrupt timeout 20000",
                "text --json tail=40",
                "await seq 101 timeout 20000",
                "await idle 2000 timeout 20000",
                "text --json tail=40",
                "key if=Do.you.want.to.proceed 1",
                "await seq 102 timeout 20000",
                "await idle 2000 timeout 20000",
                "text --json tail=40",
                "await seq 103 timeout 20000",
                "await idle 2000 timeout 20000",
                "text --json tail=40",
                // EVENT question: wait for the screen to move past it.
                "await seq 104 timeout 20000",
                "await idle 2000 timeout 20000",
                "text --json tail=40",
                "await seq 105 timeout 20000",
                "await idle 2000 timeout 20000",
                "text --json tail=40",
                // EVENT idle: the screen never moves again; it is not re-read.
                "await seq 106 timeout 20000",
                "await seq 106 timeout 20000",
                "await seq 106 timeout 20000",
                "await seq 106 timeout 20000",
            ],
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

    /// The server as it is: `await idle 2000` waits straight through a short
    /// busy spell, so no read sees the worker busy between two points. A
    /// screen that moved with nothing the point is made of changed (a footer
    /// tick) prints no second EVENT; the same last row after a real turn — the
    /// manager's row and a new done row above it — is a new point.
    #[test]
    fn a_point_is_new_when_its_transcript_moved_whatever_the_loop_saw() {
        let ticked = {
            let mut r = question_screen();
            let last = r.len() - 1;
            r[last] = "  ? for shortcuts                      tick 2".to_string();
            r
        };
        let mut m = Mock::new(true, vec![question_screen(), ticked]);
        m.idle_skips_busy = true;
        m.vanish_after = Some(1);
        let (lines, code) = watch_lines(&mut m, &auto(30, None));
        assert_eq!(
            lines,
            [
                "EVENT question seq=101 ⏺ Keep the harness or rewrite it?",
                "EXIT session gone (await seq 102 failed: aterm-ctl: ERR exited)",
            ]
        );
        assert_eq!(code, 1);

        let first = framed(&[
            "❯ keep going",
            "",
            "⏺ Done.",
            "",
            "✻ Cogitated for 4s · done 10:40 AM",
            "",
        ]);
        let again = framed(&[
            "❯ keep going",
            "",
            "⏺ Done.",
            "",
            "✻ Cogitated for 4s · done 10:40 AM",
            "",
            "❯ and the docs",
            "",
            "⏺ Done.",
            "",
            "✻ Cogitated for 3s · done 10:40 AM",
            "",
        ]);
        let mut m = Mock::new(true, vec![first, busy_screen(), again]);
        m.idle_skips_busy = true;
        m.vanish_after = Some(0);
        let (lines, _) = watch_lines(&mut m, &auto(30, None));
        assert_eq!(
            lines,
            [
                "EVENT idle seq=101 ⏺ Done.",
                "EVENT idle seq=103 ⏺ Done.",
                "EXIT session gone (await seq 103 failed: aterm-ctl: ERR exited)",
            ]
        );
        assert_eq!(
            m.requests.iter().filter(|r| r.starts_with("text")).count(),
            2,
            "two reads, neither of them busy: {:?}",
            m.requests
        );
    }

    /// The reviewers' live cases, each a new point after a busy spell no read
    /// saw: the same `git push` asked again after it failed; a second Edit box
    /// for the same file; the same hunk's file with a new hunk and nothing new
    /// above it; a plain shell back at its `$` after another command; a retry
    /// that hit the limit wall again.
    #[test]
    fn a_new_point_that_looks_like_the_last_one_is_still_reported() {
        let pushed = [
            "⏺ Pushing the fix.",
            "",
            "⏺ Bash(git push origin main)",
            "  ⎿  error: failed to push some refs (rejected: fetch first)",
            "",
            "⏺ The remote moved; retrying the push.",
        ];
        let updated = [
            "⏺ Update(crates/foo/src/lib.rs)",
            "  ⎿  Updated crates/foo/src/lib.rs with 1 addition and 1 removal",
        ];
        let mut second_site = updated.to_vec();
        second_site.extend(["", "⏺ And the second site in the same file."]);
        let retried = framed(&[
            "⏺ Running the suite again.",
            "  ⎿  You've hit your session limit · resets 7:30pm (America/Los_Angeles)",
            "",
            "✻ Worked for 3m 2s · done 5:02 PM",
            "",
            "❯ continue",
            "  ⎿  You've hit your session limit · resets 7:30pm (America/Los_Angeles)",
            "",
            "✻ Churned for 0s · done 5:02 PM",
            "",
        ]);
        // (A shell has no busy footer, so the first read on a modern host is
        // taken before the output settled; an older host's first wait is the
        // idle one, which keeps this script's first screen its first point.)
        for (case, modern, script, want) in [
            (
                "a retried push",
                true,
                vec![
                    bash_box(&["⏺ Pushing the fix."], "git push origin main"),
                    busy_over(&pushed[..4]),
                    bash_box(&pushed, "git push origin main"),
                ],
                "EVENT prompt kind=bash classify=not-read-only:git push command=git push origin main",
            ),
            (
                "a second edit to one file",
                true,
                vec![
                    edit_box(&["⏺ Fixing the constant."], "2"),
                    busy_over(&updated),
                    edit_box(&second_site, "3"),
                ],
                "EVENT prompt kind=edit classify=- command=crates/foo/src/lib.rs",
            ),
            (
                "a new hunk, nothing new above it",
                true,
                vec![
                    edit_box(&["⏺ Fixing the constant."], "2"),
                    busy_screen(),
                    edit_box(&["⏺ Fixing the constant."], "3"),
                ],
                "EVENT prompt kind=edit classify=- command=crates/foo/src/lib.rs",
            ),
            (
                "a shell back at its prompt",
                false,
                vec![
                    rows(&["$ make", "ok", "$"]),
                    rows(&["$ make", "ok", "$ ls", "a b c", "$"]),
                ],
                "EVENT idle $",
            ),
            (
                "a retry into the wall",
                true,
                vec![limited_screen(), busy_screen(), retried],
                "EVENT limited message=You've hit your session limit · resets 7:30pm \
                 (America/Los_Angeles) reset=7:30pm (America/Los_Angeles)",
            ),
        ] {
            let mut m = Mock::new(modern, script);
            m.idle_skips_busy = true;
            m.vanish_after = Some(0);
            let (lines, code) = watch_lines(
                &mut m,
                &SuperviseOpts {
                    max: Duration::from_secs(30),
                    ..SuperviseOpts::default()
                },
            );
            let got = decisions(&lines);
            assert_eq!(&got[..2], [want, want], "{case}: {lines:?}");
            assert!(got[2].starts_with("EXIT session gone"), "{case}: {lines:?}");
            assert_eq!(got.len(), 3, "{case}: {lines:?}");
            assert_eq!(code, 1, "{case}");
            assert!(
                !m.requests.iter().any(|r| r.starts_with("key")),
                "{case}: nothing pressed: {:?}",
                m.requests
            );
        }
    }

    /// A read-only prompt handed over (it came back after two approvals) is
    /// not pressed when the screen moves under it without the worker running;
    /// the other review points print their own summaries.
    #[test]
    fn watch_summarises_each_kind_of_review_point() {
        let mut m = Mock::new(
            true,
            vec![
                write_prompt(),
                busy_screen(),
                limited_screen(),
                busy_screen(),
                workflow_box(),
            ],
        );
        m.vanish_after = Some(0);
        let (lines, _) = watch_lines(&mut m, &auto(30, None));
        assert_eq!(
            lines,
            [
                "EVENT prompt seq=101 kind=bash classify=not-read-only:rm command=rm -rf target",
                "EVENT limited seq=103 message=You've hit your session limit · resets 7:30pm \
                 (America/Los_Angeles) reset=7:30pm (America/Los_Angeles)",
                "EVENT prompt seq=105 kind=workflow classify=- command=Fan out the 12 benchmark \
                 families to 4 agents and collect the standings table.",
                "EXIT session gone (await seq 105 failed: aterm-ctl: ERR exited)",
            ]
        );
        assert!(m.presses().is_empty(), "{:?}", m.requests);

        // Two approvals, then the same read is the manager's — and stays the
        // manager's while the screen moves under it with the worker idle.
        let mut m = Mock::new(
            true,
            vec![
                bash_one_row(),
                busy_screen(),
                bash_one_row(),
                busy_screen(),
                bash_one_row(),
                bash_one_row(),
            ],
        );
        m.vanish_after = Some(0);
        let (lines, _) = watch_lines(&mut m, &auto(30, None));
        assert_eq!(
            lines,
            [
                "APPROVED seq=101 git log --oneline -5",
                "APPROVED seq=103 git log --oneline -5",
                "EVENT prompt seq=105 kind=bash classify=read-only command=git log --oneline -5",
                "EXIT session gone (await seq 106 failed: aterm-ctl: ERR exited)",
            ]
        );
        assert_eq!(m.presses().len(), 2, "{:?}", m.requests);
    }

    /// A worker without Claude Code's composer — a build — whose output never
    /// held still for the idle window is still running: no review point until
    /// it pauses. The one read taken while it streamed is not handed over.
    #[test]
    fn a_frameless_screen_still_changing_is_busy_until_it_settles() {
        let mut m = Mock::new(
            true,
            vec![
                rows(&["$ make", "compiling a"]),
                rows(&["$ make", "compiling a", "compiling b", "done", "$"]),
            ],
        );
        // `await gone` latches at once (no busy footer on a shell); the first
        // `await idle` runs out its step: the output never paused.
        m.replies.push_back(ok("OK gone 100\n"));
        m.replies.push_back(timeout());
        let mut s = Session::new(&mut m, None);
        let (out, code) = s.supervise(&auto(30, None)).expect("supervise");
        assert_eq!(code, 0);
        assert_eq!(out, "idle\n--\n$ make\ncompiling a\ncompiling b\ndone\n$\n");
        assert_eq!(
            m.requests,
            [
                "await gone esc.to.interrupt timeout 20000",
                "text --json tail=40",
                "await idle 2000 timeout 20000",
                "text --json tail=40",
                "await seq 102 timeout 20000",
                "await idle 2000 timeout 20000",
                "text --json tail=40",
            ]
        );
        // What `phase` would print for such a turn if the budget ran out.
        let streaming = Turn {
            phase: Phase::Busy,
            screen: Screen {
                rows: rows(&["$ make", "compiling a"]),
                ..Screen::default()
            },
            timed_out: true,
        };
        assert_eq!(
            render_phase(&streaming, &[]),
            "busy\nreason whole screen, no composer frame: output still changing\n"
        );
    }

    /// The budget bounds every action: a turn read at or after the deadline
    /// is the TIMEOUT — `watch` prints no EVENT before it, and `supervise`
    /// presses nothing, whatever the phase — with the last read's lines.
    #[test]
    fn nothing_happens_after_the_budget() {
        let spent = SuperviseOpts {
            max: Duration::ZERO,
            ..SuperviseOpts::default()
        };
        let mut m = Mock::new(true, vec![busy_screen()]);
        let (lines, code) = watch_lines(&mut m, &spent);
        assert_eq!((lines, code), (vec!["TIMEOUT".to_string()], EXIT_TIMEOUT));

        let mut m = Mock::new(true, vec![idle_screen()]);
        let (lines, code) = watch_lines(&mut m, &spent);
        assert_eq!((lines, code), (vec!["TIMEOUT".to_string()], EXIT_TIMEOUT));

        let mut m = Mock::new(true, vec![bash_one_row()]);
        let mut s = Session::new(&mut m, None);
        let (out, code) = s.supervise(&auto(0, None)).expect("supervise");
        assert_eq!(code, EXIT_TIMEOUT);
        assert!(out.starts_with("TIMEOUT\nprompt\nkind bash\n"), "{out}");
        assert!(m.presses().is_empty(), "{:?}", m.requests);

        let mut m = Mock::new(true, vec![idle_screen()]);
        let mut s = Session::new(&mut m, None);
        let (out, code) = s.supervise(&spent).expect("supervise");
        assert_eq!(code, EXIT_TIMEOUT);
        assert!(out.starts_with("TIMEOUT\nidle\n--\n"), "{out}");
    }

    #[test]
    fn watch_lines_are_cut_at_160_characters() {
        let long = "x".repeat(400);
        assert_eq!(clip(&long).chars().count(), LINE_CHARS);
        assert_eq!(clip("a\tb\nc"), "a b c");
        assert_eq!(exit_reason("ERR halted\n"), "ERR halted");
        assert_eq!(
            exit_reason("text --json tail=40 failed: aterm-ctl: ERR no such session"),
            "session gone (text --json tail=40 failed: aterm-ctl: ERR no such session)"
        );
        assert_eq!(
            exit_reason("await seq 633 failed: aterm-ctl: ERR exited"),
            "session gone (await seq 633 failed: aterm-ctl: ERR exited)"
        );
    }

    #[test]
    fn utc_stamp_is_iso_8601() {
        assert_eq!(utc_stamp(0), "1970-01-01T00:00:00Z");
        assert_eq!(utc_stamp(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(utc_stamp(1_757_527_218), "2025-09-10T18:00:18Z");
    }
}
