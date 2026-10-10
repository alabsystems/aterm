// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The resume command a person is told to run: `claude --resume <id>`, where
//! `<id>` is the conversation this process holds, read from Claude Code's own
//! record (`<claude dir>/sessions/<pid>.json`). Never `claude --continue`,
//! which resumes the directory's newest conversation, a sibling tab's when
//! several Claude Codes share a directory.
//!
//! * [`command`]: pure, the line from the process's argv and conversation id,
//!   flags carried as the relaunch carries them
//!   ([`super::upgrade::rewrite_argv`]); an unknown flag leaves the bare line,
//!   and a launch not resumable in place (`--worktree`, `--print`, …) gets
//!   none.
//! * [`of_entry`]: the line for one live process from the footer's registry
//!   entry ([`super::footer::read_pid`]), guarded against pid reuse; `None`
//!   when the record is missing or another process's.
//! * [`shorter`]: the forms a caller whose byte budget cannot hold the line
//!   steps down through — its permission mode alone, then [`bare`], the line
//!   without carried flags: a shorter command, never a cut one (a cut id names
//!   no conversation, or another's).
//!
//! The command names `claude`, as a person types it, whatever path the
//! process ran from; and it does not `cd`: `--resume` finds a conversation
//! under the directory it was started in, which is the tab's own shell's
//! directory in every launch a person makes from that shell. Nor does it
//! carry the LOGIN a relaunch chose for the session: a remedy says in one
//! clause which login the command runs under instead
//! ([`SHELL_LOGIN_CLAUSE`], with the reasons).

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
    /// The window's host relaunches it on its own conversation once the
    /// restart has ended it (`[harness] relaunch`): nothing for a person to
    /// type ([`RELAUNCH_WORDS`]), and the restart a remedy names is the
    /// explicit one ([`restart_command`]), whose exit the host reads as the
    /// restart's — a bare `signal term`'s reads as the agent's own. Said
    /// only where that relaunch would come
    /// ([`super::relaunch::resumes_on_exit`]), since it replaces the command.
    Relaunch,
    /// The window's host carries the explicit restart ([`restart_command`])
    /// and brings back what it ends
    /// ([`super::relaunch::carries_restart`]), but on what it cannot promise
    /// in advance — a Codex: its thread where that has a rollout by then,
    /// else a fresh start. The remedy names that restart and nothing after
    /// it: neither [`RELAUNCH_WORDS`] nor a resume line a person would run
    /// over the relaunch (review of `fix/restart-advice`, 2026-10-08: a
    /// hosted Codex was promised its conversation).
    Act,
}

/// What a remedy says in place of a command when the host relaunches the
/// agent itself ([`AfterRestart::Relaunch`]).
pub const RELAUNCH_WORDS: &str = "aterm relaunches it on its conversation";

/// THE EXPLICIT RESTART a remedy names wherever the window's host relaunches
/// the agent ([`AfterRestart::Relaunch`]; design of 2026-10-08): `restart
/// pid=<pid> if-stalled` — the leader the stall was published for, else no
/// `pid=` (the tab's foreground group) — and, with the tab's id `sid` (`@`
/// or not), as a person runs it: `aterm ctl @<sid> restart pid=<pid>
/// if-stalled`. Its stamp is written before its signal, so the exit is the
/// restart's whoever reads it, and it ends a program that lives through its
/// `SIGTERM` — where a bare `signal term`'s exit reads as the agent's own.
#[must_use]
pub fn restart_command(sid: Option<&str>, pid: Option<u32>) -> String {
    let args = pid.map_or_else(
        || "restart if-stalled".to_string(),
        |pid| format!("restart pid={pid} if-stalled"),
    );
    match sid {
        Some(sid) => format!("aterm ctl @{} {args}", sid.trim_start_matches('@')),
        None => args,
    }
}

/// WHICH LOGIN A RESUME TYPED BY HAND RUNS UNDER, for a session the harness
/// relaunched under a login of its choosing (an account move,
/// `docs/DESIGN-claude-accounts-2026-10-01.md` §6): the clause a remedy adds
/// to the sentence that hands a person [`command`]. The command itself
/// carries NO environment, by decision:
///
/// * every surface that prints it has a hard budget — the tab's mark is 200
///   bytes, a remedy's sentence 90 characters — and steps down
///   ([`shorter`]) rather than cut ([`never_cut`]). A login directory is
///   some hundred bytes (up to `accounts::DIR_MAX`), so the variable would be
///   the first thing dropped, silently, exactly where it mattered;
/// * the label is the only name an account has in anything aterm prints,
///   and the variable's value is a directory under the person's home;
/// * landing on the shell's login is no failure. The conversation resumes
///   (the Claude home did not move), and a Claude started by hand is
///   decided again at its first turn end like any other session.
///
/// So the one true sentence is said instead: typed at a prompt, the command
/// runs under THAT SHELL's login — whatever variable that shell exports, or
/// the default one — which need not be the login the session last ran under.
pub const SHELL_LOGIN_CLAUSE: &str = ", under that shell's own Claude login";

/// [`SHELL_LOGIN_CLAUSE`] for a process whose own exec environment named a
/// login (`login`: the raw `CLAUDE_SECURESTORAGE_CONFIG_DIR`, as
/// `relaunch::Snapshot::login` keeps it — set, empty or not), else nothing:
/// a process that had no such variable ran under its shell's login already,
/// and its resume line needs no word about it.
#[must_use]
pub fn login_clause(login: Option<&str>) -> &'static str {
    match login {
        Some(_) => SHELL_LOGIN_CLAUSE,
        None => "",
    }
}

/// How a reason names the resume line after a restart:
/// `<…>, then resume with <line><…>` — the memory wall's escalation
/// (`supervise::policy::turn_end`'s `memory_restart`). [`never_cut`] finds
/// the line by it.
pub const THEN_RESUME_WITH: &str = ", then resume with ";

/// `render(text)`, a renderer that may cut its input to fit a cap, with the
/// resume line `text` names after [`THEN_RESUME_WITH`] kept whole or not at
/// all: the first of `text`, `text` with each of the line's shorter forms
/// ([`shorter`]), and `text` without the clause, whose rendering still holds
/// its line whole. Text naming no such line is rendered as it is. A cut id
/// names no conversation, or another's.
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
    for step in shorter(line) {
        let candidate = text.replacen(line, &step, 1);
        if let Some(out) = whole(&candidate, &step) {
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

/// The PERMISSION MODE form of a line [`command`] made: the line with only
/// the flags that set its permission mode ([`upgrade::permission_argv`]).
/// `None` for any other text, and for a line whose launch set no permission
/// mode (its bare form, [`bare_of`], is that). The line's words are read
/// back as [`upgrade::quote`] wrote them, so a flag's value that only looks
/// like a permission flag is none.
#[must_use]
pub fn mode_of(line: &str) -> Option<String> {
    let (_, id) = line.rsplit_once(" --resume ")?;
    let words = super::upgrade_drive::last_command(line);
    if words.first().map(String::as_str) != Some("claude") {
        return None;
    }
    command(&upgrade::permission_argv(&words), id).filter(|mode| bare(id).as_ref() != Some(mode))
}

/// THE SHORTER FORMS OF A LINE [`command`] MADE, in the order a surface
/// whose budget cannot hold it whole steps down through them, never cutting
/// it (a cut id names no conversation, or another's): its permission mode
/// alone ([`mode_of`]), then the conversation alone ([`bare_of`]) — each
/// only where it is shorter than the one before; none for any other text.
/// The permission mode is the last flag a line gives up (round three of the
/// joint audit of 2026-10-01): stepping straight down to the bare line
/// dropped `--dangerously-skip-permissions` with every other flag, and an
/// orchestrator was told a line that brings it back in another permission
/// mode — one started in `--permission-mode plan`, out of it.
#[must_use]
pub fn shorter(line: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for step in mode_of(line).into_iter().chain(bare_of(line)) {
        if step.len() < out.last().map_or(line.len(), String::len) {
            out.push(step);
        }
    }
    out
}

/// The resume line for the live Claude Code process whose registry `entry`
/// (`<claude dir>/sessions/<pid>.json`) the footer read parsed
/// ([`super::footer::read_pid`], through
/// [`super::footer::session_of_pid`]), and `argv`, the process's own: the
/// line every place tells a person to run
/// ([`super::upgrade_drive::resume_command_kept`], over the harness's state
/// root `state`).
///
/// The record must be this process's: when `started` (the process's kernel
/// start, unix seconds) is known, the entry's `procStart` must name that same
/// second ([`super::footer::lstart_utc`]); a record left under a reused pid,
/// or one with no `procStart`, fails. A wrong line here would be typed and
/// resume another conversation, so the caller then prints no command.
#[must_use]
pub fn of_entry(
    argv: &[String],
    entry: &super::footer::SessionEntry,
    started: Option<u64>,
    state: Option<&Path>,
) -> Option<String> {
    if started.is_some_and(|started| entry.proc_start != Some(started)) {
        return None;
    }
    super::upgrade_drive::resume_command_kept(state, argv, &entry.session_id)
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
