// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The resume command a person is told to run.

use std::path::Path;

use super::upgrade::{self, ArgvRefusal, Dialect};

/// What follows a frozen agent's restart, as a remedy says it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum AfterRestart {
    /// Nothing is known to resume it with.
    #[default]
    Unknown,
    /// A person runs this line ([`command`]) once the restart has ended it.
    Command(String),
    /// The host relaunches it on its own conversation once the restart has ended it (`[harness] relaunch`).
    Relaunch,
    /// The host carries the explicit restart ([`restart_command`], [`super::relaunch::carries_restart`]).
    Act,
}

/// What a remedy says in place of a command when the host relaunches the agent itself ([`AfterRestart::Relaunch`]).
pub const RELAUNCH_WORDS: &str = "aterm relaunches it on its conversation";

/// The explicit restart a remedy names wherever the host relaunches the agent.
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

/// Clause added when handing [`command`] for a session relaunched under an account move.
pub const SHELL_LOGIN_CLAUSE: &str = ", under that shell's own Claude login";

/// How a reason names the resume line after a restart.
pub const THEN_RESUME_WITH: &str = ", then resume with ";

/// Apply `render(text)` while keeping the resume command after [`THEN_RESUME_WITH`] whole or omitting it.
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
/// `--resume <id>` after it, the pair [`command`] always ends with.
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

/// Build the command to resume `session_id` with flags carried from `argv` via [`upgrade::rewrite_argv`].
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

/// `claude --resume <session_id>`.
#[must_use]
pub fn bare(session_id: &str) -> Option<String> {
    upgrade::is_session_id(session_id).then(|| format!("claude --resume {session_id}"))
}

/// `line` ([`command`]) with `env` in front of it.
#[must_use]
pub fn with_env(env: &[(&str, &str)], line: &str) -> Option<String> {
    let mut out = String::new();
    for (name, value) in env {
        if !env_name(name) || value.chars().any(char::is_control) {
            return None;
        }
        let plain = !value.is_empty()
            && value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'/' | b'-'));
        out.push_str(name);
        out.push('=');
        if plain {
            out.push_str(value);
        } else {
            out.push_str(&upgrade::quote(Dialect::Zsh, value));
        }
        out.push(' ');
    }
    out.push_str(line);
    Some(out)
}

/// Compute the `CLAUDE_CONFIG_DIR` prefix for a process given `config`, `claude_dir`, and `user_home`.
#[must_use]
pub fn home_env(
    user_home: &Path,
    config: Option<&str>,
    claude_dir: Option<&Path>,
) -> Vec<(&'static str, String)> {
    if let Some(config) = config {
        return vec![(super::accounts::HOME_ENV, config.to_string())];
    }
    claude_dir
        .filter(|dir| !super::upgrade_drive::same_dir(dir, &user_home.join(".claude")))
        .and_then(super::upgrade_drive::homedir_holding)
        .map(|homedir| (super::accounts::HOMEDIR_ENV, homedir))
        .into_iter()
        .collect()
}

/// Whether `name` can be an environment variable's in a line: `[A-Z0-9_]+`.
fn env_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
}

/// A told line split at its program: the `NAME=value ` words [`with_env`]
/// put in front of it, and the command from `claude` on.
fn split_env(line: &str) -> (&str, &str) {
    let mut at = 0;
    while let Some(len) = env_word_len(&line[at..]) {
        at += len;
    }
    line.split_at(at)
}

/// The length of the `NAME=value ` word `rest` begins with, its space
/// included — a value read as [`with_env`] quotes it — or `None` where it
/// begins with no such word.
fn env_word_len(rest: &str) -> Option<usize> {
    let eq = rest.find('=')?;
    if !env_name(&rest[..eq]) {
        return None;
    }
    let mut quoted = false;
    let mut chars = rest[eq + 1..].char_indices();
    while let Some((i, c)) = chars.next() {
        match c {
            '\'' => quoted = !quoted,
            '\\' if !quoted => {
                chars.next();
            }
            ' ' if !quoted => return Some(eq + 1 + i + 1),
            _ => {}
        }
    }
    None
}

/// The bare form of a line [`command`] made, [`bare`] of the id after its last `--resume`, the Claude home in front of it kept ([`with_env`]).
#[must_use]
pub fn bare_of(line: &str) -> Option<String> {
    let (env, line) = split_env(line);
    let (_, id) = line.rsplit_once(" --resume ")?;
    bare(id).map(|bare| format!("{env}{bare}"))
}

/// The permission-mode form of a line [`command`] made.
#[must_use]
pub fn mode_of(line: &str) -> Option<String> {
    let (env, line) = split_env(line);
    let (_, id) = line.rsplit_once(" --resume ")?;
    let words = super::upgrade_drive::last_command(line);
    if words.first().map(String::as_str) != Some("claude") {
        return None;
    }
    command(&upgrade::permission_argv(&words), id)
        .filter(|mode| bare(id).as_ref() != Some(mode))
        .map(|mode| format!("{env}{mode}"))
}

/// The shorter forms of a line [`command`] made, in the order a surface whose budget cannot hold it steps down through them.
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

/// Build the resume line for a live Claude Code process from `entry`, `argv`, and harness `state` ([`super::upgrade_drive::resume_command_kept`]).
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
                    | aterm_phase::Program::Gemini
                    | aterm_phase::Program::Generic => false,
                })
    });
    if script_is_claude { &argv[1..] } else { argv }
}

/// Whether a word must be quoted for a POSIX shell (or fish): anything but
/// the characters no shell treats specially.
fn needs_quotes(word: &str) -> bool {
    word.is_empty()
        || !word
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_./:=@%+,".contains(&b))
}

#[cfg(test)]
#[path = "resume_tests.rs"]
mod tests;
