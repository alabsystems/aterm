// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One screen reader PER PROGRAM, and the one place that picks it.
//!
//! Everything else in this crate reads Claude Code's screen. Run on a shell,
//! it reads a quoted `Esc to cancel` as a box and a line ending in `?` as a
//! question; run on Codex, it reads Codex's trust gate as `idle` (both
//! measured). The fabric bridge and the GUI rim ran it on every session all
//! the same. So a caller names the program ([`identify`]) and asks that
//! program's reader ([`ScreenReader`]):
//!
//! * [`ClaudeReader`] — this crate's Claude Code grammar, behind the trait;
//! * [`CodexReader`] — Codex's grammar ([`crate::codex`], measured on codex
//!   0.156.1): its boxes with their roles, the status row and the streaming
//!   answer as busy, a turn's end row as idle or a question, its `■` walls
//!   and interrupt. A screen whose last message's head has scrolled away
//!   with no end in sight is `idle` NOT authoritatively;
//! * [`GenericReader`] — any other program: never a prompt, a question or a
//!   wall, always `idle`, never authoritative. The screen alone says
//!   nothing program-neutral about whether a build or a REPL is working;
//!   aterm's own `status phase=` (running | quiet) is the authority there.
//!
//! A policy that acts on a phase — a continuation typed into an `idle`
//! worker, an answer pressed into a `prompt` — requires
//! [`Reading::phase_authoritative`]: a reader's default is not evidence.
//!
//! [`read`] is the whole reading in one call ([`Reading`]);
//! [`Program::supervisable`] is which programs the supervisor hosts.

use crate::anchors::{ANCHORS, Anchor, CODEX_ANCHORS, guard_regex};
use crate::codex;
use crate::phase::{
    Phase, busy_signal, composer_draft, context_left, has_composer_frame, survey_open, worker_phase,
};
use crate::prompt::{PromptKind, PromptV2, parse_prompt_v2};
use crate::turn::{continuation_suggestion, goal_active, interrupted, said_tail};
use crate::wall::{Wall, memory_wall, wall};

/// The program a reader is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Program {
    Claude,
    Codex,
    /// Anything else, or nothing named.
    Generic,
}

impl Program {
    /// The lowercase word the CLI prints.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Program::Claude => "claude",
            Program::Codex => "codex",
            Program::Generic => "generic",
        }
    }

    /// Whether the supervisor hosts this program: one whose reader is
    /// MEASURED — Claude Code, and Codex since its reader passed its
    /// captures ([`crate::codex::fixtures`]). Never the generic reader.
    #[must_use]
    pub fn supervisable(self) -> bool {
        matches!(self, Program::Claude | Program::Codex)
    }

    /// This program's reader.
    #[must_use]
    pub fn reader(self) -> &'static dyn ScreenReader {
        match self {
            Program::Claude => &ClaudeReader,
            Program::Codex => &CodexReader,
            Program::Generic => &GenericReader,
        }
    }
}

/// Everything one screen says, from one program's reader.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reading {
    pub program: Program,
    pub phase: Phase,
    /// Whether [`Self::phase`] is the reader's evidence rather than its
    /// default: `false` for the generic reader always, and for the Codex
    /// reader where nothing on the screen says whether a turn runs — each
    /// says `idle` there because it cannot tell working from waiting, not
    /// because it saw either. A policy that acts on `idle` (a continuation)
    /// or on `prompt` requires it.
    pub phase_authoritative: bool,
    /// The wall the last turn ended on — `None` while a box is up, and while
    /// the worker is (hard) busy only a HEALTH wall
    /// ([`ScreenReader::health_wall`]: Claude Code's critical-memory banner,
    /// which it draws under a running spinner). A turn that ended on `API
    /// Error: 529` reads [`Phase::Idle`] with a wall here.
    pub wall: Option<Wall>,
    /// The box, when [`Self::phase`] is [`Phase::Prompt`].
    pub prompt: Option<PromptV2>,
    pub survey: bool,
    pub context_left: Option<u8>,
    /// The program's own suggestion for the next message (needs the
    /// cursor's column; `None` without it).
    pub suggestion: Option<String>,
    pub goal_active: bool,
    /// The worker's last words ([`crate::turn::said_tail`]).
    pub said_tail: Option<String>,
    /// A PERSON stopped the last turn (Esc): the next message is theirs
    /// ([`crate::turn::interrupted`], [`crate::codex::interrupted`]).
    pub interrupted: bool,
    /// No turn yet ([`ScreenReader::fresh`]: [`crate::turn::fresh`],
    /// [`crate::codex::fresh`]): an idle point here ended nothing — a
    /// session nobody has asked anything is not a worker stopped short.
    pub fresh: bool,
    /// The composer as `(caret row, lines)`, the caret row's text first —
    /// typed text or the dim placeholder, which only the cursor tells apart
    /// (at column 2 of the caret row: the placeholder, for both programs);
    /// `None` with no composer on the screen (a box, a shell).
    pub composer: Option<(usize, Vec<String>)>,
}

/// A program's screen grammar.
pub trait ScreenReader: Sync {
    fn program(&self) -> Program;
    fn phase(&self, rows: &[String]) -> Phase;
    fn prompt(&self, rows: &[String]) -> Option<PromptV2>;
    fn wall(&self, rows: &[String]) -> Option<Wall>;
    /// A wall that says the PROGRAM, not the turn, is stuck — read even
    /// while the worker is hard busy, where [`Self::wall`] is not asked (a
    /// retry notice under a live spinner is not a wall yet; a process past
    /// saving is one however busy its spinner looks). `None` by default:
    /// only a reader that knows its program's own such banner has one.
    fn health_wall(&self, _rows: &[String]) -> Option<Wall> {
        None
    }
    fn survey(&self, rows: &[String]) -> bool;
    /// THE AGENT'S OWN BACKGROUND WORK, AND NOTHING ELSE, KEEPS THIS SCREEN
    /// BUSY: its turn is over, the composer drawn, and what runs on is work
    /// it started (a dynamic workflow, a background agent, a shell, a Codex
    /// background terminal) — `Some(what runs)`. `None` for a live turn, an
    /// idle screen, a box, or a program with no such read (the default).
    fn background_wait(&self, _rows: &[String]) -> Option<&'static str> {
        None
    }
    fn context(&self, rows: &[String]) -> Option<u8>;
    fn suggestion(&self, rows: &[String], cursor_col: usize) -> Option<String>;
    fn goal_active(&self, rows: &[String]) -> bool;
    fn said_tail(&self, rows: &[String]) -> Option<String>;
    fn interrupted(&self, rows: &[String]) -> bool;
    /// No turn yet: the program's launch card on the screen and, under it,
    /// no message and nothing said — nobody has asked the session anything.
    fn fresh(&self, rows: &[String]) -> bool;
    fn composer(&self, rows: &[String]) -> Option<(usize, Vec<String>)>;
    /// The glyph the composer's caret row starts with (`❯` Claude Code, `›`
    /// Codex): what a guard on typed text anchors to. `None`: no composer.
    fn caret(&self) -> Option<char>;
    /// An Enter that arrives within a burst of typed text is taken as a
    /// NEWLINE, not a submit (Codex's paste guard, measured): typed text is
    /// submitted by a `turn` that settles first, never by an Enter right
    /// behind the text.
    fn paste_guard(&self) -> bool;
    /// A guard (the wire's regex: spaces as `.`) matching the row a RUNNING
    /// turn shows, and only that row — its leaving ends the turn (`await
    /// gone`). `None`: nothing on this program's screen says a turn runs.
    fn busy_guard(&self) -> Option<String>;
    /// The strings this program's guards name.
    fn anchors(&self) -> &'static [Anchor];
    /// Whether `phase`, read from `rows`, is evidence ([`Reading::
    /// phase_authoritative`]).
    fn phase_authoritative(&self, rows: &[String], phase: &Phase) -> bool;

    /// The whole reading.
    fn read(&self, rows: &[String], cursor_col: Option<usize>) -> Reading {
        let phase = self.phase(rows);
        let phase_authoritative = self.phase_authoritative(rows, &phase);
        let prompt = (phase == Phase::Prompt)
            .then(|| self.prompt(rows))
            .flatten();
        let hard_busy = phase == Phase::Busy && busy_signal(rows).is_some_and(|b| !b.soft);
        // Never under a box (its answer is the human's, and the banner's
        // place under one is not measured); under a hard busy only a health
        // wall; otherwise every wall.
        let wall = if phase == Phase::Prompt {
            None
        } else if hard_busy {
            self.health_wall(rows)
        } else {
            self.wall(rows)
        };
        Reading {
            program: self.program(),
            phase,
            phase_authoritative,
            wall,
            prompt,
            survey: self.survey(rows),
            context_left: self.context(rows),
            suggestion: cursor_col.and_then(|c| self.suggestion(rows, c)),
            goal_active: self.goal_active(rows),
            said_tail: self.said_tail(rows),
            interrupted: self.interrupted(rows),
            fresh: self.fresh(rows),
            composer: self.composer(rows),
        }
    }
}

/// Claude Code: this crate's grammar.
#[derive(Debug, Clone, Copy)]
pub struct ClaudeReader;

impl ScreenReader for ClaudeReader {
    fn program(&self) -> Program {
        Program::Claude
    }
    fn phase(&self, rows: &[String]) -> Phase {
        worker_phase(rows)
    }
    fn prompt(&self, rows: &[String]) -> Option<PromptV2> {
        parse_prompt_v2(rows)
    }
    fn wall(&self, rows: &[String]) -> Option<Wall> {
        wall(rows)
    }
    fn health_wall(&self, rows: &[String]) -> Option<Wall> {
        memory_wall(rows)
    }
    fn survey(&self, rows: &[String]) -> bool {
        survey_open(rows)
    }
    fn background_wait(&self, rows: &[String]) -> Option<&'static str> {
        crate::phase::background_wait(rows)
    }
    fn context(&self, rows: &[String]) -> Option<u8> {
        context_left(rows)
    }
    fn suggestion(&self, rows: &[String], cursor_col: usize) -> Option<String> {
        continuation_suggestion(rows, cursor_col)
    }
    fn goal_active(&self, rows: &[String]) -> bool {
        goal_active(rows)
    }
    fn said_tail(&self, rows: &[String]) -> Option<String> {
        said_tail(rows)
    }
    fn interrupted(&self, rows: &[String]) -> bool {
        interrupted(rows)
    }
    fn fresh(&self, rows: &[String]) -> bool {
        crate::turn::fresh(rows)
    }
    /// Inside its frame only: a `❯` row with no frame around it is the
    /// transcript's (a user's message), never the composer — a box replaces
    /// the frame.
    fn composer(&self, rows: &[String]) -> Option<(usize, Vec<String>)> {
        has_composer_frame(rows)
            .then(|| composer_draft(rows))
            .flatten()
    }
    fn caret(&self) -> Option<char> {
        Some('❯')
    }
    fn paste_guard(&self) -> bool {
        false
    }
    /// The busy footer's `esc to interrupt`, under the spinner.
    fn busy_guard(&self) -> Option<String> {
        Some(guard_regex(crate::anchor("busy.interrupt")))
    }
    fn anchors(&self) -> &'static [Anchor] {
        ANCHORS
    }
    fn phase_authoritative(&self, _: &[String], _: &Phase) -> bool {
        true
    }
}

/// Codex: [`crate::codex`]'s grammar (module header).
#[derive(Debug, Clone, Copy)]
pub struct CodexReader;

impl ScreenReader for CodexReader {
    fn program(&self) -> Program {
        Program::Codex
    }
    fn phase(&self, rows: &[String]) -> Phase {
        codex::phase(rows).0
    }
    fn prompt(&self, rows: &[String]) -> Option<PromptV2> {
        codex::prompt(rows)
    }
    fn wall(&self, rows: &[String]) -> Option<Wall> {
        codex::wall(rows)
    }
    fn survey(&self, _: &[String]) -> bool {
        false
    }
    fn background_wait(&self, rows: &[String]) -> Option<&'static str> {
        codex::background_wait(rows)
    }
    fn context(&self, rows: &[String]) -> Option<u8> {
        codex::context_left(rows)
    }
    /// Codex draws no suggestion for the next message: its composer's dim
    /// text is a stock placeholder (`Ask Codex to do anything`), measured.
    fn suggestion(&self, _: &[String], _: usize) -> Option<String> {
        None
    }
    fn goal_active(&self, _: &[String]) -> bool {
        false
    }
    fn said_tail(&self, rows: &[String]) -> Option<String> {
        codex::said_tail(rows)
    }
    fn interrupted(&self, rows: &[String]) -> bool {
        codex::interrupted(rows)
    }
    fn fresh(&self, rows: &[String]) -> bool {
        codex::fresh(rows)
    }
    fn composer(&self, rows: &[String]) -> Option<(usize, Vec<String>)> {
        codex::composer_draft(rows)
    }
    fn caret(&self) -> Option<char> {
        Some('›')
    }
    /// Measured on 0.156.1 ([`crate::codex`], "TYPING INTO THE COMPOSER").
    fn paste_guard(&self) -> bool {
        true
    }
    /// The status row's tail, `esc to interrupt)` (`• Working (6s • esc to
    /// interrupt)`): the question dialog's footer names the same hint with
    /// no parenthesis, and must not read as a turn running.
    fn busy_guard(&self) -> Option<String> {
        Some(format!(
            "{}\\)",
            guard_regex(crate::anchors::anchor_text("codex.busy.interrupt"))
        ))
    }
    fn anchors(&self) -> &'static [Anchor] {
        CODEX_ANCHORS
    }
    fn phase_authoritative(&self, rows: &[String], _: &Phase) -> bool {
        codex::phase(rows).1
    }
}

/// Any other program (module header).
#[derive(Debug, Clone, Copy)]
pub struct GenericReader;

impl ScreenReader for GenericReader {
    fn program(&self) -> Program {
        Program::Generic
    }
    fn phase(&self, _: &[String]) -> Phase {
        Phase::Idle
    }
    fn prompt(&self, _: &[String]) -> Option<PromptV2> {
        None
    }
    fn wall(&self, _: &[String]) -> Option<Wall> {
        None
    }
    fn survey(&self, _: &[String]) -> bool {
        false
    }
    fn context(&self, _: &[String]) -> Option<u8> {
        None
    }
    fn suggestion(&self, _: &[String], _: usize) -> Option<String> {
        None
    }
    fn goal_active(&self, _: &[String]) -> bool {
        false
    }
    fn said_tail(&self, _: &[String]) -> Option<String> {
        None
    }
    fn interrupted(&self, _: &[String]) -> bool {
        false
    }
    fn fresh(&self, _: &[String]) -> bool {
        false
    }
    fn composer(&self, _: &[String]) -> Option<(usize, Vec<String>)> {
        None
    }
    fn caret(&self) -> Option<char> {
        None
    }
    fn paste_guard(&self) -> bool {
        false
    }
    fn busy_guard(&self) -> Option<String> {
        None
    }
    fn anchors(&self) -> &'static [Anchor] {
        &[]
    }
    fn phase_authoritative(&self, _: &[String], _: &Phase) -> bool {
        false
    }
}

/// Shells, pagers, viewers and editors: a session whose foreground program
/// is one of these runs no agent, whatever its screen shows (a `cat`, a
/// `less` or a `vim` of a saved Claude screen included).
const NOT_AGENTS: &[&str] = &[
    "zsh", "bash", "sh", "fish", "dash", "ksh", "tcsh", "csh", "nu", "pwsh", "login", "less",
    "more", "most", "man", "vim", "nvim", "vi", "view", "emacs", "nano", "tail", "head", "cat",
    "bat", "watch",
];

/// The reader for a session. The PROGRAM NAME first — the basename of the
/// first word of `program` (a `status` `program=`/`detail=` value), a login
/// shell's leading `-` dropped: `claude` → Claude, `codex` → Codex, a shell,
/// a pager, a viewer or an editor ([`NOT_AGENTS`]) → Generic. Then, for no
/// name or any other one (a Claude Code started as `node`, `ssh` to a host
/// running one, a session adopted with no name), the screen: Claude Code's
/// composer frame or one of its boxes (the trust dialog replaces the frame)
/// → Claude; a Codex choice box, or Codex's composer empty or under its
/// status row (`codex::is_codex_screen`) → Codex; else Generic. A Codex
/// with no name and a draft in its composer, no turn running, reads
/// Generic until it next runs or shows a box.
///
/// `program` must name the session's FOREGROUND process — the one drawing
/// the screen. The session's root shell names every Claude Code started
/// from it as a shell, and nothing would be supervised.
#[must_use]
pub fn identify(program: Option<&str>, rows: &[String]) -> &'static dyn ScreenReader {
    match program.and_then(program_of) {
        Some(Program::Claude) => return &ClaudeReader,
        Some(Program::Codex) => return &CodexReader,
        _ => {}
    }
    if program
        .map(program_word)
        .is_some_and(|name| NOT_AGENTS.contains(&name.as_str()))
    {
        return &GenericReader;
    }
    // A footerless box of no kind aterm-phase names (a setup dialog) is
    // drawn the same by any program's menu — a shell script's `❯ 1. dev`
    // under a rule and a title — so it names Claude Code only where the
    // program's name already did (the harness round-3 review of 2026-09-24).
    let names_claude = |p: PromptV2| p.kind != PromptKind::Other || crate::prompt::footed(rows, &p);
    if has_composer_frame(rows) || parse_prompt_v2(rows).is_some_and(names_claude) {
        &ClaudeReader
    } else if codex::codex_box(rows).is_some() || codex::is_codex_screen(rows) {
        &CodexReader
    } else {
        &GenericReader
    }
}

/// How a program that was restarted resumes the conversation it lost — the
/// words Claude Code's own critical-memory banner ends on (`… restart and
/// resume with claude --continue`), for a supervisor's escalation and the
/// server's own attention to repeat. `None` for a program with no such
/// command this crate knows. The words are a command, not a screen anchor.
#[must_use]
pub fn resume_hint(program: Program) -> Option<&'static str> {
    match program {
        Program::Claude => Some("claude --continue"),
        Program::Codex | Program::Generic => None,
    }
}

/// [`identify`], then that reader's [`ScreenReader::read`].
#[must_use]
pub fn read(program: Option<&str>, rows: &[String], cursor_col: Option<usize>) -> Reading {
    identify(program, rows).read(rows, cursor_col)
}

/// The program word of a `program=` / `detail=` value: the basename of its
/// first word, lowercased, a login shell's `-` dropped.
/// THE ONE NAME TABLE: the agent a program name is, by its name alone —
/// the basename of its first word, a login shell's `-` dropped: `claude` and
/// `claude-code` are Claude Code, `codex` is Codex, anything else is no
/// agent BY NAME (it may still be one by its screen: [`identify`],
/// [`may_host_agent`]). The server's agent verdict, its program shim rule
/// and the in-GUI supervisor host all ask this, never a table of their own.
#[must_use]
pub fn program_of(program: &str) -> Option<Program> {
    match program_word(program).as_str() {
        "claude" | "claude-code" => Some(Program::Claude),
        "codex" => Some(Program::Codex),
        _ => None,
    }
}

/// The script runtimes an agent runs AS without renaming itself — an
/// npm/bun install of Claude Code starts under `node` or `bun`: a program so
/// named may be identified as an agent by its SCREEN (Claude Code's frame, a
/// Codex box or composer: [`identify`]). A shell, a pager or `cat` showing a
/// captured screen may not.
pub const AGENT_RUNTIMES: &[&str] = &["node", "bun", "deno"];

/// Whether `program` is one of [`AGENT_RUNTIMES`].
#[must_use]
pub fn may_host_agent(program: &str) -> bool {
    AGENT_RUNTIMES.contains(&program_word(program).as_str())
}

fn program_word(program: &str) -> String {
    let head = program.split_whitespace().next().unwrap_or(program);
    let base = head.rsplit('/').next().unwrap_or(head);
    base.trim_start_matches('-').to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prompt::fixtures::{
        BOX_BASH_TOUCH, CODEX_TRUST, END_529, GOAL_ACTIVE_SUGGESTION, MEMORY_BANNER_BUSY,
        MEMORY_BANNER_IDLE, TRUST, bash_one_row, composer, rows, screen,
    };
    use crate::prompt::{CancelEffect, PromptKind, Role, Select};
    use crate::wall::WallKind;

    /// codex 0.155.1's trust gate, MEASURED (carried over from the
    /// supervisor's screen profile when `ada0c5abf` deleted it).
    fn codex_0_155_trust() -> Vec<String> {
        rows(&[
            "> You are in /Users//x/aterm",
            "",
            "  Do you trust the contents of this directory? Working with untrusted contents comes with higher risk of",
            "  prompt injection. Trusting the directory allows project-local config, hooks, and exec policies to load.",
            "",
            "› 1. Yes, continue",
            "  2. No, quit",
            "",
            "  Press enter to continue",
        ])
    }

    fn shell_asking() -> Vec<String> {
        rows(&[
            "% ./configure",
            "checking for gcc... gcc",
            "Overwrite the existing config?",
        ])
    }

    /// The program name decides first; with none (or one that names no
    /// agent), the screen does — Claude Code's frame or one of its boxes, a
    /// Codex choice box, else the generic reader. A shell's name wins over
    /// a Claude-looking screen it is merely printing.
    #[test]
    fn identify_takes_the_program_name_then_the_screen() {
        let framed = {
            let mut r = rows(&["⏺ Done.", ""]);
            r.extend(composer("  ? for shortcuts"));
            r
        };
        let p = |name: Option<&str>, r: &[String]| identify(name, r).program();
        assert_eq!(p(Some("claude"), &shell_asking()), Program::Claude);
        assert_eq!(
            p(Some("/opt/bin/codex --full-auto"), &framed),
            Program::Codex
        );
        assert_eq!(p(Some("-zsh"), &framed), Program::Generic);
        assert_eq!(p(Some("bash"), &bash_one_row()), Program::Generic);
        assert_eq!(p(None, &framed), Program::Claude);
        assert_eq!(p(Some("node"), &framed), Program::Claude);
        assert_eq!(p(None, &screen(TRUST)), Program::Claude, "a box, no frame");
        assert_eq!(p(None, &screen(BOX_BASH_TOUCH)), Program::Claude);
        assert_eq!(p(None, &screen(CODEX_TRUST)), Program::Codex);
        assert_eq!(p(None, &codex_0_155_trust()), Program::Codex);
        assert_eq!(p(None, &shell_asking()), Program::Generic);
        assert_eq!(p(Some("python3"), &shell_asking()), Program::Generic);
        // A pager or an editor showing a saved Claude screen is not Claude.
        for viewer in ["less", "/usr/bin/vim", "tail -f log", "watch"] {
            assert_eq!(
                p(Some(viewer), &bash_one_row()),
                Program::Generic,
                "{viewer}"
            );
        }
        assert_eq!(
            p(Some("ssh"), &bash_one_row()),
            Program::Claude,
            "the control"
        );
    }

    /// A reader's default is not evidence: the generic reader's `idle` and
    /// the Codex reader's `idle` where nothing says whether a turn runs (the
    /// last message's head scrolled away, no end row) are not
    /// authoritative; the Codex box, its status row and every Claude reading
    /// are.
    #[test]
    fn a_default_idle_is_not_authoritative() {
        let headless = rows(&[
            "  the tail of an answer whose head scrolled away",
            "",
            "› Ask Codex to do anything",
            "",
            "  gpt-5 · ~/src",
        ]);
        let r = read(Some("codex"), &headless, None);
        assert_eq!(r.phase, Phase::Idle);
        assert!(!r.phase_authoritative);
        let working = rows(&[
            "• Working (12s • esc to interrupt)",
            "",
            "› Ask Codex to do anything",
            "",
            "  gpt-5 · ~/src",
        ]);
        let r = read(Some("codex"), &working, None);
        assert_eq!((r.phase, r.phase_authoritative), (Phase::Busy, true));
        assert!(read(Some("codex"), &screen(CODEX_TRUST), None).phase_authoritative);
        let g = read(Some("zsh"), &shell_asking(), None);
        assert_eq!(g.phase, Phase::Idle);
        assert!(!g.phase_authoritative);
        let c = read(Some("claude"), &screen(END_529), None);
        assert_eq!(c.phase, Phase::Idle);
        assert!(c.phase_authoritative, "the control");
    }

    /// The generic reader never says prompt or question: a shell screen
    /// ending in `?` — which Claude Code's reader DOES call a question, the
    /// control — and a shell printing a Claude box.
    #[test]
    fn the_generic_reader_is_never_a_question_or_a_prompt() {
        let r = shell_asking();
        assert_eq!(ClaudeReader.phase(&r), Phase::Question, "the control");
        assert_eq!(GenericReader.phase(&r), Phase::Idle);
        let reading = read(Some("zsh"), &bash_one_row(), Some(2));
        assert_eq!(reading.program, Program::Generic);
        assert_eq!(reading.phase, Phase::Idle);
        assert_eq!(reading.prompt, None);
        assert_eq!(reading.wall, None);
        assert_eq!(
            ClaudeReader.phase(&bash_one_row()),
            Phase::Prompt,
            "the control"
        );
    }

    /// Codex's trust gate (both measured builds) is a trust prompt: its
    /// folder, `Trust and continue` / `Yes, continue` as the trust and
    /// `Quit` / `No, quit` as the exit, the cursor on the first, and `esc
    /// quit` read as an exit. Claude Code's reader on the same screen sees
    /// nothing — why the dispatch.
    #[test]
    fn the_codex_reader_reads_its_trust_gate_by_role() {
        let r = screen(CODEX_TRUST);
        let reading = read(Some("codex"), &r, None);
        assert_eq!(reading.phase, Phase::Prompt);
        let p = reading.prompt.expect("the box");
        assert_eq!(p.kind, PromptKind::Trust);
        assert_eq!(p.title, "Folder access");
        assert!(p.path.as_deref().is_some_and(|f| f.ends_with("/lv/work3")));
        let labels: Vec<&str> = p.options.iter().map(|o| o.label.as_str()).collect();
        assert_eq!(labels, vec!["Trust and continue", "Quit"]);
        let roles: Vec<Role> = p.options.iter().map(|o| o.role).collect();
        assert_eq!(roles, vec![Role::Trust, Role::Exit]);
        assert!(p.options[0].focused && !p.options[1].focused);
        // Its cursor and Enter choose (`enter continue`); a digit changes
        // nothing on it (measured under the host, 2026-09-24).
        assert_eq!(p.select, Select::ArrowsEnter);
        assert_eq!(p.cancel.map(|c| c.effect), Some(CancelEffect::Exit));

        let old = codex_0_155_trust();
        let p = CodexReader.prompt(&old).expect("the 0.155.1 box");
        assert_eq!(p.kind, PromptKind::Trust);
        assert!(
            p.title.starts_with("Do you trust the contents"),
            "{}",
            p.title
        );
        let roles: Vec<Role> = p.options.iter().map(|o| o.role).collect();
        assert_eq!(roles, vec![Role::Trust, Role::Exit]);
        assert_eq!(p.cancel, None, "`Press enter to continue` names no Esc");

        assert_eq!(CodexReader.phase(&shell_asking()), Phase::Idle);
        // Claude's own reader reads the Codex gate as idle: why the dispatch.
        assert_eq!(ClaudeReader.phase(&r), Phase::Idle, "the control");
    }

    /// What a supervisor TYPING into a program needs of its reader: the
    /// composer on the screen (Claude Code's only inside its frame — a `❯`
    /// row under a box is the transcript's), the caret glyph a guard anchors
    /// to, whether an Enter right behind typed text is taken as a newline
    /// (Codex's paste guard), and the row whose leaving ends a turn — Codex's
    /// status row, never its question footer's same hint. NEGATIVE CONTROLS:
    /// the generic reader has none of them; `Program::reader` is each one.
    #[test]
    fn each_reader_says_how_to_type_into_its_program() {
        use crate::codex::fixtures as cx;
        let matches = |guard: &str, row: &str| {
            let re = guard.replace("\\)", ")").replace('.', " ");
            row.contains(&re)
        };
        let claude = Program::Claude.reader();
        let codex = Program::Codex.reader();
        assert_eq!(claude.program(), Program::Claude);
        assert_eq!(codex.program(), Program::Codex);
        assert_eq!(Program::Generic.reader().program(), Program::Generic);
        assert_eq!((claude.caret(), codex.caret()), (Some('❯'), Some('›')));
        assert!(codex.paste_guard() && !claude.paste_guard());
        let idle = {
            let mut r = rows(&["⏺ Done.", ""]);
            r.extend(composer("  ? for shortcuts"));
            r
        };
        assert!(claude.composer(&idle).is_some(), "the framed composer");
        assert_eq!(
            claude.composer(&screen(BOX_BASH_TOUCH)),
            None,
            "a box replaces the frame: no composer"
        );
        let busy = codex.busy_guard().expect("codex's busy row");
        let status_row = screen(cx::BUSY)
            .into_iter()
            .find(|r| r.starts_with("• ") && r.contains("esc to interrupt"))
            .expect("the status row");
        assert!(matches(&busy, &status_row), "{busy} / {status_row}");
        let question_footer = screen(cx::QUESTION)
            .into_iter()
            .rfind(|r| r.contains("esc to interrupt"))
            .expect("the question's footer");
        assert!(
            !matches(&busy, &question_footer),
            "the question's footer is no running turn: {question_footer}"
        );
        assert!(claude.busy_guard().is_some());
        let g = GenericReader;
        assert_eq!(
            (g.caret(), g.busy_guard(), g.paste_guard()),
            (None, None, false)
        );
    }

    /// One read, whole: the 529 end of turn is idle WITH its wall; the
    /// measured `/goal` screen is busy (a workflow runs), carries the armed
    /// goal and Claude Code's suggestion, and no wall.
    #[test]
    fn a_reading_carries_the_wall_and_the_turn_end_facts() {
        let r = read(None, &screen(END_529), Some(2));
        assert_eq!(r.program, Program::Claude);
        assert_eq!(r.phase, Phase::Idle);
        assert_eq!(r.wall.map(|w| w.kind), Some(WallKind::Overloaded));
        assert_eq!(
            r.said_tail.as_deref(),
            Some("Suites are running; I'll report the number when it lands.")
        );

        let g = read(Some("claude"), &screen(GOAL_ACTIVE_SUGGESTION), Some(2));
        assert_eq!(g.phase, Phase::Busy);
        assert!(g.goal_active);
        assert_eq!(g.suggestion.as_deref(), Some("keep going"));
        assert_eq!(g.wall, None);
        assert_eq!(
            read(Some("claude"), &screen(GOAL_ACTIVE_SUGGESTION), None).suggestion,
            None
        );

        // A box: the prompt is read, no wall is.
        let b = read(Some("claude"), &screen(TRUST), None);
        assert_eq!(b.phase, Phase::Prompt);
        assert_eq!(b.prompt.map(|p| p.kind), Some(PromptKind::Trust));
        assert_eq!(b.wall, None);
    }

    /// The 2026-09-24 incident's screen: the spinner still running, so the
    /// worker reads BUSY — and the reading carries Claude Code's
    /// critical-memory banner as a memory wall all the same, the one wall a
    /// hard busy keeps (`ScreenReader::health_wall`). Idle it is the same
    /// wall. NEGATIVE CONTROLS: the banner row blanked, leaving only the
    /// draft that quotes it, reads no wall; a box on the screen reads none
    /// however placed the banner is; and no reader but Claude Code's has one.
    #[test]
    fn the_memory_banner_is_a_wall_even_under_a_busy_spinner() {
        let busy = screen(MEMORY_BANNER_BUSY);
        for program in [Some("claude"), None] {
            let r = read(program, &busy, None);
            assert_eq!(r.program, Program::Claude);
            assert_eq!(r.phase, Phase::Busy, "{program:?}");
            assert!(busy_signal(&busy).is_some_and(|b| !b.soft), "hard busy");
            assert_eq!(
                r.wall.map(|w| w.kind),
                Some(WallKind::Memory),
                "{program:?}"
            );
        }
        let idle = read(Some("claude"), &screen(MEMORY_BANNER_IDLE), None);
        assert_eq!(idle.phase, Phase::Idle);
        assert_eq!(idle.wall.map(|w| w.kind), Some(WallKind::Memory));

        let at = memory_wall(&busy).expect("the banner row").row;
        let mut quoted = busy.clone();
        quoted[at] = String::new();
        let r = read(Some("claude"), &quoted, None);
        assert_eq!(r.phase, Phase::Busy);
        assert_eq!(r.wall, None, "only the draft's quote is left");

        // A box with the banner placed right above the composer's rule: the
        // banner is there to read (the control), the reading carries none.
        let mut boxed = bash_one_row();
        let top = boxed
            .iter()
            .rposition(|row| row.starts_with('─'))
            .expect("rules")
            - 2;
        let width = boxed[top].chars().count();
        let banner = busy[at].trim_start();
        boxed.insert(top, format!("{banner:>w$}", w = width - 2));
        assert!(memory_wall(&boxed).is_some(), "the control");
        let r = read(Some("claude"), &boxed, None);
        assert_eq!(r.phase, Phase::Prompt);
        assert_eq!(r.wall, None);

        // The generic reader (`cat` of the screen) and Codex's have no
        // health wall.
        assert_eq!(read(Some("zsh"), &busy, None).wall, None);
        assert_eq!(CodexReader.health_wall(&busy), None);
        assert_eq!(GenericReader.health_wall(&busy), None);
        assert_eq!(ClaudeReader.health_wall(&busy).map(|w| w.row), Some(at));
    }

    #[test]
    fn only_claude_code_has_a_resume_hint() {
        assert_eq!(resume_hint(Program::Claude), Some("claude --continue"));
        assert_eq!(resume_hint(Program::Codex), None);
        assert_eq!(resume_hint(Program::Generic), None);
        // The banner's own last words are the hint.
        let banner = memory_wall(&screen(MEMORY_BANNER_BUSY)).expect("the banner");
        assert!(
            banner
                .message
                .ends_with(resume_hint(Program::Claude).unwrap())
        );
    }
}
