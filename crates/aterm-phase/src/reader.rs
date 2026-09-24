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
//! * [`CodexReader`] — a stub from the ONE Codex screen measured (its
//!   folder-trust gate, codex 0.156.1): it sees a numbered choice box and
//!   reports it as a prompt with no option roles, so nothing approves by
//!   it and every Codex box is escalated. Nothing else is read: any other
//!   Codex screen is `idle` with [`Reading::phase_authoritative`] `false`;
//! * [`GenericReader`] — any other program: never a prompt, a question or a
//!   wall, always `idle`, never authoritative. The screen alone says
//!   nothing program-neutral about whether a build or a REPL is working;
//!   aterm's own `status phase=` (running | quiet) is the authority there.
//!
//! A policy that acts on a phase — a continuation typed into an `idle`
//! worker, an answer pressed into a `prompt` — requires
//! [`Reading::phase_authoritative`]: a reader's default is not evidence.
//!
//! [`read`] is the whole reading in one call ([`Reading`]).

use crate::anchors::{ANCHORS, Anchor};
use crate::phase::{
    Phase, busy_signal, context_left, has_composer_frame, leading_spaces, survey_open, worker_phase,
};
use crate::prompt::{
    Cancel, CancelEffect, Opt, PromptKind, PromptV2, Role, Select, parse_prompt_v2,
};
use crate::turn::{continuation_suggestion, goal_active, said_tail};
use crate::wall::{Wall, wall};

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
}

/// Everything one screen says, from one program's reader.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reading {
    pub program: Program,
    pub phase: Phase,
    /// Whether [`Self::phase`] is the reader's evidence rather than its
    /// default: `false` for the generic reader always, and for the Codex
    /// reader outside its choice box — each says `idle` there because it
    /// cannot tell working from waiting, not because it saw either. A policy
    /// that acts on `idle` (a continuation) or on `prompt` requires it.
    pub phase_authoritative: bool,
    /// The wall the last turn ended on — `None` while a box is up or the
    /// worker is (hard) busy. A turn that ended on `API Error: 529` reads
    /// [`Phase::Idle`] with a wall here.
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
}

/// A program's screen grammar.
pub trait ScreenReader: Sync {
    fn program(&self) -> Program;
    fn phase(&self, rows: &[String]) -> Phase;
    fn prompt(&self, rows: &[String]) -> Option<PromptV2>;
    fn wall(&self, rows: &[String]) -> Option<Wall>;
    fn survey(&self, rows: &[String]) -> bool;
    fn context(&self, rows: &[String]) -> Option<u8>;
    fn suggestion(&self, rows: &[String], cursor_col: usize) -> Option<String>;
    fn goal_active(&self, rows: &[String]) -> bool;
    fn said_tail(&self, rows: &[String]) -> Option<String>;
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
        let wall = if phase == Phase::Prompt || hard_busy {
            None
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
    fn survey(&self, rows: &[String]) -> bool {
        survey_open(rows)
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
    fn anchors(&self) -> &'static [Anchor] {
        ANCHORS
    }
    fn phase_authoritative(&self, _: &[String], _: &Phase) -> bool {
        true
    }
}

/// Codex: a numbered choice box and nothing else (module header).
#[derive(Debug, Clone, Copy)]
pub struct CodexReader;

impl ScreenReader for CodexReader {
    fn program(&self) -> Program {
        Program::Codex
    }
    fn phase(&self, rows: &[String]) -> Phase {
        if codex_box(rows).is_some() {
            Phase::Prompt
        } else {
            Phase::Idle
        }
    }
    fn prompt(&self, rows: &[String]) -> Option<PromptV2> {
        codex_prompt(rows)
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
    fn anchors(&self) -> &'static [Anchor] {
        &[]
    }
    /// Only its choice box is read; every other screen is `idle` by default.
    fn phase_authoritative(&self, _: &[String], phase: &Phase) -> bool {
        *phase == Phase::Prompt
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
/// → Claude; a Codex choice box → Codex; else Generic. The frame test is
/// `aterm-agent`'s `harness::profile::identify` frame test, ported.
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
    if has_composer_frame(rows) || parse_prompt_v2(rows).is_some() {
        &ClaudeReader
    } else if codex_box(rows).is_some() {
        &CodexReader
    } else {
        &GenericReader
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
/// Codex box). A shell, a pager or `cat` showing a captured screen may not.
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

/// A Codex numbered choice box, as `(first option row, footer row)`: a row in
/// column 0 `› 1. <label>` (the cursor), the rest of the options under it,
/// and within three rows of the last one a hint row — `enter continue · esc
/// quit` (codex 0.156.1) or `Press enter to continue` (codex 0.155.1, from
/// `aterm-agent`'s profile tests). Measured on the trust gate only.
fn codex_box(rows: &[String]) -> Option<(usize, usize)> {
    let first = rows.iter().rposition(|r| {
        r.strip_prefix('›')
            .is_some_and(|rest| codex_option(&format!("  {rest}")).is_some())
    })?;
    let mut last = first;
    while last + 1 < rows.len() && codex_option(&rows[last + 1]).is_some() {
        last += 1;
    }
    let footer = (last + 1..(last + 4).min(rows.len())).find(|&i| {
        let t = rows[i].trim().to_lowercase();
        t.starts_with("enter ") || t.starts_with("press enter") || t.contains(" · esc ")
    })?;
    Some((first, footer))
}

/// `› 1. Trust and continue` / `  2. Quit` → `(1, "Trust and continue")`.
fn codex_option(row: &str) -> Option<(u8, String)> {
    let t = row.trim_start();
    let t = t.strip_prefix('›').map_or(t, str::trim_start);
    let digits: String = t.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() || digits.len() > 2 {
        return None;
    }
    let rest = t[digits.len()..].strip_prefix(". ")?;
    Some((digits.parse().ok()?, rest.trim().to_string()))
}

/// A Codex box as a [`PromptV2`] of kind [`PromptKind::Other`]: its title
/// (the first row of the block above the options, up to a row in column 0),
/// its options with NO roles ([`Role::Other`]), chosen by digit, and the
/// footer's `esc quit` as an exit. What the options do is not measured, so
/// nothing may approve by them.
fn codex_prompt(rows: &[String]) -> Option<PromptV2> {
    let (first, footer) = codex_box(rows)?;
    let top = (0..first)
        .rev()
        .find(|&i| {
            let r = &rows[i];
            !r.trim().is_empty() && leading_spaces(r) == 0
        })
        .map_or(0, |i| i + 1);
    let title_row = (top..first).find(|&i| !rows[i].trim().is_empty())?;
    let options = (first..footer)
        .filter_map(|i| {
            codex_option(&rows[i]).map(|(n, label)| Opt {
                n: Some(n),
                label,
                role: Role::Other,
                focused: rows[i].starts_with('›'),
                row: i,
            })
        })
        .collect();
    let hint = rows[footer].trim().to_lowercase();
    let cancel = hint
        .split(" · ")
        .find_map(|item| item.strip_prefix("esc "))
        .map(|verb| Cancel {
            key: "esc".to_string(),
            verb: verb.to_string(),
            effect: if verb.starts_with("quit") || verb.starts_with("exit") {
                CancelEffect::Exit
            } else {
                CancelEffect::Back
            },
        });
    Some(PromptV2 {
        kind: PromptKind::Other,
        title: rows[title_row].trim().to_string(),
        command_rows: Vec::new(),
        description_rows: 0,
        gutter: false,
        command: String::new(),
        description: String::new(),
        notes: Vec::new(),
        auto_deny: None,
        path: None,
        options,
        select: Select::Digits,
        cancel,
        span: (title_row, footer),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prompt::fixtures::{
        BOX_BASH_TOUCH, CODEX_TRUST, END_529, GOAL_ACTIVE_SUGGESTION, TRUST, bash_one_row,
        composer, rows, screen,
    };
    use crate::wall::WallKind;

    /// codex 0.155.1's trust gate, MEASURED (copied from `aterm-agent`'s
    /// `harness/profile_tests.rs` `CODEX_SCREEN`).
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
    /// the Codex reader's `idle` outside its box are not authoritative; the
    /// Codex box and every Claude reading are.
    #[test]
    fn a_default_idle_is_not_authoritative() {
        let working = rows(&[
            "• Working (12s • esc to interrupt)",
            "",
            "› Ask Codex to do anything",
        ]);
        let r = read(Some("codex"), &working, None);
        assert_eq!(r.phase, Phase::Idle);
        assert!(!r.phase_authoritative);
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

    /// Codex's trust gate (both measured builds) is a prompt whose options
    /// carry NO role — nothing may approve by them — with the cursor on the
    /// first and `esc quit` read as an exit. Any other Codex screen is idle.
    #[test]
    fn the_codex_reader_escalates_its_boxes_and_approves_nothing() {
        let r = screen(CODEX_TRUST);
        let reading = read(Some("codex"), &r, None);
        assert_eq!(reading.phase, Phase::Prompt);
        let p = reading.prompt.expect("the box");
        assert_eq!(p.kind, PromptKind::Other);
        assert_eq!(p.title, "Folder access");
        let labels: Vec<&str> = p.options.iter().map(|o| o.label.as_str()).collect();
        assert_eq!(labels, vec!["Trust and continue", "Quit"]);
        assert!(p.options.iter().all(|o| o.role == Role::Other));
        assert!(p.options[0].focused && !p.options[1].focused);
        assert_eq!(p.select, Select::Digits);
        assert_eq!(p.cancel.map(|c| c.effect), Some(CancelEffect::Exit));

        let old = codex_0_155_trust();
        let p = CodexReader.prompt(&old).expect("the 0.155.1 box");
        assert!(
            p.title.starts_with("Do you trust the contents"),
            "{}",
            p.title
        );
        assert_eq!(p.options.len(), 2);
        assert_eq!(p.cancel, None, "`Press enter to continue` names no Esc");

        assert_eq!(CodexReader.phase(&shell_asking()), Phase::Idle);
        // Claude's own reader reads the Codex gate as idle: why the dispatch.
        assert_eq!(ClaudeReader.phase(&r), Phase::Idle, "the control");
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
}
