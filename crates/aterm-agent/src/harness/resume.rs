// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The resume command a person is told to run: `claude --resume <id>`, `<id>` the conversation
//! this process holds, read from Claude Code's own record (`<claude dir>/sessions/<pid>.json`).
//! Never `claude --continue`, which resumes the directory's newest conversation, a sibling tab's
//! when several Claude Codes share a directory.
//!
//! [`command`] carries flags as the relaunch does; [`codex_command`] makes a Codex's line,
//! `codex resume <kept flags> <thread>`, behind `CODEX_HOME=<value>` where the TUI was started
//! with one; [`of_entry`] guards against pid reuse; [`shorter`] steps either line down to fit a
//! budget, never cutting it (a cut id names no conversation, or another's) and never dropping a
//! Codex line's home.
//!
//! The command names its program as a person types it, whatever path the process ran from. It
//! does not `cd`: `--resume` finds a conversation under the directory it was started in, the tab's
//! shell's directory for every launch made from that shell. It does not carry the login a
//! relaunch chose ([`SHELL_LOGIN_CLAUSE`]).

use std::path::Path;

use super::upgrade::{self, ArgvRefusal, Dialect};
use super::upgrade_codex;

/// What follows a frozen agent's restart, as a remedy says it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum AfterRestart {
    /// Nothing is known to resume it with — no record, another process's, or
    /// a program with no such command: the remedy names no command.
    #[default]
    Unknown,
    /// A person runs this line ([`command`]) once the restart has ended it.
    Command(String),
    /// The host relaunches it on its own conversation once the restart has ended it (`[harness]
    /// relaunch`): nothing to type ([`RELAUNCH_WORDS`]), and the remedy names the explicit restart
    /// ([`restart_command`]), whose exit the host reads as the restart's. Said only where that
    /// relaunch would come ([`super::relaunch::resumes_on_exit`]).
    Relaunch,
    /// The host carries the explicit restart ([`restart_command`]) and brings back what it ends
    /// ([`super::relaunch::carries_restart`]), but cannot promise how: a Codex resumes its thread
    /// where that has a rollout by then, else starts afresh. The remedy names only the restart.
    Act,
}

/// What a remedy says in place of a command when the host relaunches the
/// agent itself ([`AfterRestart::Relaunch`]).
pub const RELAUNCH_WORDS: &str = "aterm relaunches it on its conversation";

/// The explicit restart a remedy names wherever the host relaunches the agent:
/// `restart pid=<pid> if-stalled` (`pid` the leader the stall was published for; without it, the
/// tab's foreground group), as `aterm ctl @<sid> restart …` when `sid` (`@` or not) is given. Its
/// stamp is written before its signal, so the exit reads as the restart's, and it ends a program
/// that lives through `SIGTERM`; a bare `signal term`'s exit reads as the agent's own.
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

/// The clause a remedy adds when it hands a person [`command`] for a session the harness relaunched
/// under a login of its choosing (an account move, `docs/DESIGN-claude-accounts-2026-10-01.md` §6):
/// typed at a prompt, the command runs under that shell's login. The command carries no
/// environment, by decision:
///
/// * every surface that prints it has a hard budget (the tab's mark 200 bytes, a remedy's sentence
///   90 characters) and steps down ([`shorter`]) rather than cut ([`never_cut`]); a login directory
///   is up to [`super::accounts::DIR_MAX`] bytes, so the variable would be dropped first, exactly
///   where it mattered;
/// * the label is the only name an account has in anything aterm prints, and the variable's value
///   is a directory under the person's home;
/// * landing on the shell's login is no failure: the conversation resumes, and a Claude started by
///   hand is decided again at its first turn end.
pub const SHELL_LOGIN_CLAUSE: &str = ", under that shell's own Claude login";

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

/// The resume line `text` names after [`THEN_RESUME_WITH`], and the byte range of the whole clause
/// (the marker and the line): the line runs from the marker to the end of the first
/// `--resume <id>` after it, the pair [`command`] always ends with, or for a Codex's line
/// (`codex resume …`, or `CODEX_HOME=… codex resume …`) to its thread ([`codex_line_end`]).
fn named_line(text: &str) -> Option<(std::ops::Range<usize>, &str)> {
    const FLAG: &str = " --resume ";
    let at = text.find(THEN_RESUME_WITH)?;
    let start = at + THEN_RESUME_WITH.len();
    let rest = &text[start..];
    let end = if rest.starts_with(CODEX_RESUME) || rest.starts_with(CODEX_HOME_WORD) {
        codex_line_end(rest)?
    } else {
        rest.match_indices(FLAG).find_map(|(i, _)| {
            let id_at = i + FLAG.len();
            let id = rest.get(id_at..id_at + 36)?;
            upgrade::is_session_id(id).then_some(id_at + 36)
        })?
    };
    Some((at..start + end, &rest[..end]))
}

/// What a Codex's resume line starts with ([`codex_command`]), behind the
/// home it carries, if any ([`CODEX_HOME_WORD`]).
const CODEX_RESUME: &str = "codex resume ";

/// What a Codex's resume line that carries its home starts with ([`home_word`]).
const CODEX_HOME_WORD: &str = "CODEX_HOME=";

/// The end of the Codex line `rest` starts with: the longest prefix that ends in a thread id and
/// reads back as exactly such a line ([`codex_parts`]). The longest, so a flag value that is a
/// thread id does not end the line early; no longer prefix reads back, since the thread would
/// become a positional the rewrite drops.
fn codex_line_end(rest: &str) -> Option<usize> {
    const ID: usize = 36;
    (ID..=rest.len())
        .rev()
        .filter(|&end| {
            rest.get(end - ID..end)
                .is_some_and(upgrade_codex::is_thread_id)
        })
        .find(|&end| codex_parts(&rest[..end]).is_some())
}

/// The line a person runs to resume `session_id` with the flags `argv` (`argv[0]` first) was
/// launched with, carried as [`upgrade::rewrite_argv`] carries them. `None` for an id that is not
/// one, a control character in a carried word, or a launch not resumable in place; an unknown flag
/// ([`ArgvRefusal::UnknownFlag`]) gives the [`bare`] line.
///
/// A script launch (`node /…/claude …`, the managed twin's `#!/bin/sh`) has the script as
/// `argv[1]`, and its flags start after it, as [`super::relaunch::launched`] reads them.
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
    typed("claude", &flags)
}

/// The line a person runs to resume a Codex on its thread `thread` with the flags `argv`
/// (`argv[0]` first) was launched with: `codex resume <kept flags> <thread>`, carried as
/// [`upgrade_codex::rewrite_argv`] carries them and quoted as [`command`] quotes. `None` for a
/// thread that is not one, a control character in a carried word or the home, or a launch the
/// rewrite refuses (an unknown flag, `--worktree`, `--remote`). Whether the thread is the
/// process's alone is the caller's to know: a read that cannot tell it from another client's
/// names none.
///
/// `home`: the `CODEX_HOME` the TUI's exec environment set, verbatim
/// (`relaunch::CodexRun::home_env`), carried in front (`CODEX_HOME=<value> codex resume …`) as
/// the relaunch line carries it ([`upgrade_codex::line_env`]): without it `codex resume` looks
/// under `~/.codex`, where that thread is not, so no shorter form drops it. `None`: the TUI
/// had none.
#[must_use]
pub fn codex_command(argv: &[String], thread: &str, home: Option<&str>) -> Option<String> {
    if !upgrade_codex::is_thread_id(thread) {
        return None;
    }
    let line = typed(
        "codex",
        &upgrade_codex::rewrite_argv(argv, Some(thread)).ok()?,
    )?;
    Some(match home {
        Some(home) => format!("{} {line}", home_word(home)?),
        None => line,
    })
}

/// `CODEX_HOME=<value>` as a shell reads an assignment: the value quoted
/// where it needs it, never the name (a quoted `NAME=` is a command, not
/// an assignment); `None` for a control character, which no line carries.
fn home_word(home: &str) -> Option<String> {
    if home.chars().any(char::is_control) {
        return None;
    }
    let value = if needs_quotes(home) {
        upgrade::quote(Dialect::Zsh, home)
    } else {
        home.to_string()
    };
    Some(format!("{}={value}", upgrade_codex::HOME_ENV))
}

/// `program` and `words`, as a person types them: each word quoted where a
/// shell would read it otherwise ([`needs_quotes`]); `None` for a word with
/// a control character, which no line carries.
fn typed(program: &str, words: &[String]) -> Option<String> {
    let mut line = String::from(program);
    for word in words {
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

/// A Codex resume line's carried argv (`codex` and its flags, as [`codex_command`] takes them),
/// its thread and the `CODEX_HOME` it carries. `None` for any text that is not exactly such a
/// line: its words, read back as [`upgrade::quote`] wrote them, must make the same line again.
fn codex_parts(line: &str) -> Option<CodexParts> {
    let words = super::upgrade_drive::last_command(line);
    let (home, words) = match words.split_first() {
        Some((first, rest)) => match first.split_once('=') {
            Some((upgrade_codex::HOME_ENV, value)) => (Some(value.to_string()), rest),
            _ => (None, words.as_slice()),
        },
        None => return None,
    };
    let [codex, resume, flags @ .., thread] = words else {
        return None;
    };
    if codex != "codex" || resume != "resume" {
        return None;
    }
    let argv: Vec<String> = std::iter::once(codex.clone())
        .chain(flags.iter().cloned())
        .collect();
    (codex_command(&argv, thread, home.as_deref()).as_deref() == Some(line)).then(|| CodexParts {
        argv,
        thread: thread.clone(),
        home,
    })
}

/// What [`codex_parts`] reads back from a Codex's line.
struct CodexParts {
    argv: Vec<String>,
    thread: String,
    home: Option<String>,
}

impl CodexParts {
    /// The line's bare form: `codex resume <thread>`, its home kept.
    fn bare(&self) -> Option<String> {
        codex_command(&["codex".to_string()], &self.thread, self.home.as_deref())
    }
}

/// `claude --resume <session_id>`: the conversation alone, no carried flag.
/// `None` for an id that is not one.
#[must_use]
pub fn bare(session_id: &str) -> Option<String> {
    upgrade::is_session_id(session_id).then(|| format!("claude --resume {session_id}"))
}

/// The bare form of a line [`command`] made, [`bare`] of the id after its last `--resume`, or of
/// one [`codex_command`] made, `codex resume <thread>` behind the `CODEX_HOME` the line carries,
/// which it never drops. `None` for any other text.
#[must_use]
pub fn bare_of(line: &str) -> Option<String> {
    if let Some(parts) = codex_parts(line) {
        return parts.bare();
    }
    let (_, id) = line.rsplit_once(" --resume ")?;
    bare(id)
}

/// The permission-mode form of a line [`command`] or [`codex_command`] made: the line with only the
/// flags that set its permission mode ([`upgrade::permission_argv`]; for a Codex, its approval and
/// sandbox flags, [`upgrade_codex::permission_argv`]). `None` for any other text and for a launch
/// that set no permission mode. The words are read back as [`upgrade::quote`] wrote them, so a
/// value that only looks like a permission flag is none.
#[must_use]
pub fn mode_of(line: &str) -> Option<String> {
    if let Some(parts) = codex_parts(line) {
        let bare = parts.bare();
        return codex_command(
            &upgrade_codex::permission_argv(&parts.argv),
            &parts.thread,
            parts.home.as_deref(),
        )
        .filter(|mode| Some(mode) != bare.as_ref());
    }
    let (_, id) = line.rsplit_once(" --resume ")?;
    let words = super::upgrade_drive::last_command(line);
    if words.first().map(String::as_str) != Some("claude") {
        return None;
    }
    command(&upgrade::permission_argv(&words), id).filter(|mode| bare(id).as_ref() != Some(mode))
}

/// The shorter forms of a line [`command`] or [`codex_command`] made, in the order a surface whose
/// budget cannot hold it steps down through them: its permission mode alone ([`mode_of`]), then the
/// conversation alone ([`bare_of`]), each only where shorter than the one before; none for any
/// other text. The permission mode goes last: the bare line brings a session back in another
/// permission mode.
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
/// (`<claude dir>/sessions/<pid>.json`, read by [`super::footer::session_of_pid`]) and own `argv`
/// give, over the harness state root `state` ([`super::upgrade_drive::resume_command_kept`]).
///
/// When `started` (the process's kernel start, Unix seconds) is known, the entry's `procStart`
/// must name that same second: a record left under a reused pid, or one with no `procStart`,
/// fails. A wrong line here would be typed and resume another conversation.
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
                .is_some_and(|program| match program {
                    aterm_phase::Program::Claude => true,
                    aterm_phase::Program::Codex
                    | aterm_phase::Program::Agy
                    | aterm_phase::Program::Muse
                    | aterm_phase::Program::Generic => false,
                })
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
