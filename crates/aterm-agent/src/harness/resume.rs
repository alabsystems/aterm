// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE RESUME COMMAND A PERSON IS TOLD TO RUN: `claude --resume <id>`, where
//! `<id>` is the conversation THIS process holds, read from Claude Code's own
//! record of it (`<claude dir>/sessions/<pid>.json`) — never `claude
//! --continue`.
//!
//! WHY (robustness backlog item 2, 2026-09-26). Every remedy aterm printed for
//! a frozen Claude Code — the server's stall attention and the menu row
//! (`aterm-gui`'s `input_stall::attention_text`), the supervisor's frozen mail
//! (`supervise::stall::frozen_mail_text`), the memory wall's escalation, and
//! the manual — ended `then claude --continue`, one fixed string per program
//! (`aterm_phase::resume_hint`, now gone). `--continue` resumes the NEWEST
//! conversation filed under the working directory, which is not this tab's
//! whenever another Claude Code works in the same directory: measured that
//! day on the owner's Mac, four live Claude Code processes shared
//! `/Users//example/aterm`, each with its own `sessionId`
//! (`~/.claude/sessions/{32072,46976,88956,89092}.json`). A person restarting
//! a frozen tab by the letter of the remedy would have resumed a SIBLING
//! tab's live conversation — two processes writing one transcript — and the
//! frozen tab's own work would have been left behind. The relaunch the
//! harness makes itself ([`super::relaunch`]) never had this defect: it has
//! always typed `--resume <id>` with the launch's flags carried
//! ([`super::upgrade::rewrite_argv`]). This module gives the printed remedy
//! the same two facts.
//!
//! * [`command`] — PURE: the line, from the process's argv and its
//!   conversation id. The flags are carried exactly as the relaunch carries
//!   them; a flag the rewrite does not know leaves the bare `claude --resume
//!   <id>` (the conversation is what matters), and a launch the rewrite
//!   refuses as not resumable in place (`--worktree`, `--print`, …) gets no
//!   command at all — a person told to run one would be sent to the wrong
//!   place.
//! * [`of_entry`] — the line for one live process, from the registry entry
//!   the window's footer read names ([`super::footer::read_pid`]: one read
//!   of the record feeds the model, the usage and this line), with the
//!   pid-reuse guard the line needs: the record names the same pid it is
//!   filed under and, when the caller knows the process's kernel start, its
//!   `procStart` names the same second — a record with no `procStart` then
//!   proves nothing and gets no command. `None` — no command — whenever the
//!   record is missing or another process's.
//! * [`bare`] — the same line without its carried flags, for a caller whose
//!   byte budget cannot hold them (the server's keyed attention is 200 bytes):
//!   a shorter command, never a cut one — a cut id would name no
//!   conversation, or worse, another.
//!
//! The command names `claude`, as a person types it, whatever path the
//! process ran from; and it does not `cd`: `--resume` finds a conversation
//! under the directory it was started in, which is the tab's own shell's
//! directory in every launch a person makes from that shell.

use std::path::Path;

use super::upgrade::{self, ArgvRefusal, Dialect};

/// What follows a frozen agent's restart, as a remedy says it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum AfterRestart {
    /// Nothing is known to resume it with — no record, another process's, or
    /// a program with no such command: the remedy names no command.
    #[default]
    Unknown,
    /// A person runs this line ([`command`]) once the restart has ended it.
    Command(String),
    /// The window's host relaunches it on its own conversation itself once
    /// the restart has ended it (`[harness] relaunch`, U1): nothing for a
    /// person to type ([`RELAUNCH_WORDS`]). Said only where that relaunch
    /// WOULD come — the host holds the session unheld, and its plan for the
    /// launch would be made ([`super::relaunch::resumes_on_exit`]) — because
    /// it replaces the command (resume-hint review, 2026-09-26).
    Relaunch,
}

/// What a remedy says in place of a command when the host relaunches the
/// agent itself ([`AfterRestart::Relaunch`]).
pub const RELAUNCH_WORDS: &str = "aterm relaunches it on its conversation";

/// How a reason names the resume line after a restart:
/// `<…>, then resume with <line><…>` — the memory wall's escalation
/// (`supervise::policy::turn_end`'s `memory_restart`). [`never_cut`] finds
/// the line by it.
pub const THEN_RESUME_WITH: &str = ", then resume with ";

/// `render(text)` — a renderer that may CUT its input to fit a cap (the
/// supervisor's escalation: the reason clipped to 120 cells, then to what
/// the 200-byte keyed attention leaves beside its label) — with the resume
/// line `text` names after [`THEN_RESUME_WITH`] kept WHOLE or not at all:
/// the first of `text`, `text` with the line's bare form ([`bare_of`]), and
/// `text` without the clause, whose rendering still holds its line whole.
/// Text naming no such line is rendered as it is.
///
/// WHY (resume-hint review, 2026-09-26): the memory wall's escalation put
/// the full flagged line into its reason with no budget, and the render cut
/// it — measured on a `claude --dangerously-skip-permissions` launch with
/// relaunch off, the stored `meta attention=` ended `…then resume with
/// claude --dangerously-skip-permissions --resume 5f1c2d3e-4b5a-4c6d-8e7f-0a…`,
/// and a failed restart's `the restart could not be made (<step>): ` prefix
/// cut even the bare form. A cut id names no conversation — or another's.
/// The window's own remedy (`aterm-gui`'s `input_stall::attention_text`)
/// already stepped down so; this is the same ladder for any renderer.
#[must_use]
pub fn never_cut(text: &str, render: impl Fn(&str) -> String) -> String {
    let Some((clause, line)) = named_line(text) else {
        return render(text);
    };
    let whole = |candidate: &str, line: &str| {
        let out = render(candidate);
        out.contains(line).then_some(out)
    };
    if let Some(out) = whole(text, line) {
        return out;
    }
    if let Some(bare) = bare_of(line).filter(|bare| bare != line) {
        let candidate = text.replacen(line, &bare, 1);
        if let Some(out) = whole(&candidate, &bare) {
            return out;
        }
    }
    let without = format!("{}{}", &text[..clause.start], &text[clause.end..]);
    render(&without)
}

/// The resume line `text` names after [`THEN_RESUME_WITH`], and the byte
/// range of the whole clause (the marker and the line): the line runs from
/// the marker to the end of the first `--resume <id>` after it — the pair
/// [`command`] always ends with.
fn named_line(text: &str) -> Option<(std::ops::Range<usize>, &str)> {
    const FLAG: &str = " --resume ";
    let at = text.find(THEN_RESUME_WITH)?;
    let start = at + THEN_RESUME_WITH.len();
    let rest = &text[start..];
    let end = rest.match_indices(FLAG).find_map(|(i, _)| {
        let id_at = i + FLAG.len();
        let id = rest.get(id_at..id_at + 36)?;
        upgrade::is_session_id(id).then_some(id_at + 36)
    })?;
    Some((at..start + end, &rest[..end]))
}

/// The line a person runs to resume `session_id` with the flags `argv` (the
/// process's own, `argv[0]` first) was launched with, carried as
/// [`upgrade::rewrite_argv`] carries them — or `None` when no such line can be
/// said: an id that is not one ([`upgrade::is_session_id`]), a control
/// character in a carried word, or a launch the rewrite refuses as not
/// resumable in place. An argv the rewrite does not know
/// ([`ArgvRefusal::UnknownFlag`]) gives the bare line ([`bare`]).
///
/// A script launch (`node /…/claude …`, the managed twin's `#!/bin/sh`)
/// shows the interpreter as `argv[0]` and the script as `argv[1]`: the
/// script is the program, so its flags start after it, as
/// [`super::relaunch::launched`] reads them.
#[must_use]
pub fn command(argv: &[String], session_id: &str) -> Option<String> {
    if !upgrade::is_session_id(session_id) {
        return None;
    }
    let argv = program_argv(argv);
    let flags = match upgrade::rewrite_argv(argv, session_id) {
        Ok(flags) => flags,
        Err(ArgvRefusal::UnknownFlag(_)) => return bare(session_id),
        Err(ArgvRefusal::NotResumable(_)) => return None,
    };
    let mut line = String::from("claude");
    for word in &flags {
        if word.chars().any(char::is_control) {
            return None;
        }
        line.push(' ');
        if needs_quotes(word) {
            line.push_str(&upgrade::quote(Dialect::Zsh, word));
        } else {
            line.push_str(word);
        }
    }
    Some(line)
}

/// `claude --resume <session_id>`: the conversation alone, no carried flag.
/// `None` for an id that is not one.
#[must_use]
pub fn bare(session_id: &str) -> Option<String> {
    upgrade::is_session_id(session_id).then(|| format!("claude --resume {session_id}"))
}

/// The bare form ([`bare`]) of a line [`command`] made: its conversation is
/// the id after its last `--resume`. `None` for any other text.
#[must_use]
pub fn bare_of(line: &str) -> Option<String> {
    let (_, id) = line.rsplit_once(" --resume ")?;
    bare(id)
}

/// The resume line ([`command`]) for the LIVE Claude Code process whose
/// registry `entry` (`<claude dir>/sessions/<pid>.json`) the window's footer
/// read parsed ([`super::footer::read_pid`], through
/// [`super::footer::session_of_pid`]: filed under the pid and naming it),
/// and `argv` — the process's own, as the kernel holds it.
///
/// THE RECORD MUST BE THIS PROCESS'S: when `started` (the process's kernel
/// start, unix seconds) is known, the entry's `procStart` must name that same
/// second ([`super::footer::lstart_utc`]). A record a dead process left
/// behind under a reused pid fails that — and so does a record with NO
/// `procStart`, which proves nothing: the footer shows such a record's facts
/// (a wrong model is seen), but a wrong line here is TYPED, and resumes
/// another conversation. Then `None`, and the caller prints no command
/// rather than a guess. This is the rule `of_pid` held until 2026-09-28,
/// when the one-registry-read port (main's 5bebdcf36) moved the line onto
/// the footer's entry, whose own guard skips the start check for a record
/// without `procStart` (the review of that day); `of_pid`'s second read of
/// the file went with it. Its other refusal — a record missing any of the
/// fields the upgrade's full parse wants (`status`, `kind`, …) — guarded no
/// process identity, and is not kept.
#[must_use]
pub fn of_entry(
    argv: &[String],
    entry: &super::footer::SessionEntry,
    started: Option<u64>,
) -> Option<String> {
    if started.is_some_and(|started| entry.proc_start != Some(started)) {
        return None;
    }
    command(argv, &entry.session_id)
}

/// `argv` from the program on: the script of a script launch
/// ([`command`]'s doc), else the whole argv.
fn program_argv(argv: &[String]) -> &[String] {
    let script_is_claude = argv.get(1).map(Path::new).is_some_and(|p| {
        p.is_absolute()
            && p.file_name()
                .and_then(|n| n.to_str())
                .and_then(aterm_phase::program_of)
                == Some(aterm_phase::Program::Claude)
    });
    if script_is_claude { &argv[1..] } else { argv }
}

/// Whether a word must be quoted for a POSIX shell (or fish): anything but
/// the characters no shell treats specially. A flag, a model id, a UUID and
/// a plain path need none, so the common line reads as a person would type it.
fn needs_quotes(word: &str) -> bool {
    word.is_empty()
        || !word
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_./:=@%+,".contains(&b))
}

#[cfg(test)]
#[path = "resume_tests.rs"]
mod tests;
