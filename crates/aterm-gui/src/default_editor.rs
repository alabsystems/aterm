// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! amacs, aterm's own editor, as the default `EDITOR` in aterm's tabs
//! (docs/DESIGN-amacs-2026-10-19.md §8.3–8.4, slice B3).
//!
//! aterm never sets `EDITOR` over a person's own. A tab's environment carries the hint
//! `ATERM_DEFAULT_EDITOR=amacs` ([`settle`]), and the tab's shell integration exports
//! `EDITOR=amacs` once the person's startup files have run, only when `EDITOR` and `VISUAL` are
//! both still empty and `amacs` resolves on `PATH`; it then drops the hint. A session that runs
//! no startup files before its program (a `-e` command, cmd.exe) gets `EDITOR` here, by the same
//! rule over the environment it inherits; an interactive shell aterm has no script for gets
//! none, since its startup files run after anything set here and a `VISUAL` set there could not
//! stop it ([`decides`], [`settle_session`]). `VISUAL` is never set: a program that reads
//! `VISUAL` first falls through to `EDITOR`, and a later `export EDITOR=…` in a shell still
//! decides. The hint is a deny row (`aterm_types::domain::ENV_DENY_VARS`), and an inherited
//! `EDITOR=amacs` an outer aterm's tab exported is dropped too
//! (`aterm_types::domain::is_handed_down_editor`), so an aterm started from a tab derives its
//! own under its own settings.

use crate::app_config::Config;
use aterm_core::shell_integration::{ShellType, reads_default_editor_hint};
use aterm_types::domain::{ENV_ATERM_CHILD, ENV_DEFAULT_EDITOR, is_handed_down_editor};
use std::ffi::OsString;

/// The word `EDITOR` gets: the `amacs` alias of aterm's own executable, bare, because a bundle
/// path holds spaces that `sh -c "$EDITOR file"` would split.
pub(crate) const EDITOR_WORD: &str = aterm_types::domain::DEFAULT_EDITOR_WORD;

/// `[amacs]` as the window holds it: the engine's one reader's settings and its notes.
#[derive(Default, Clone, Debug, PartialEq, Eq)]
pub(crate) struct AmacsPolicy {
    pub(crate) config: amacs::config::AmacsConfig,
    pub(crate) notes: Vec<String>,
}

impl AmacsPolicy {
    /// What `text`, the whole `aterm.toml`, writes under `[amacs]`.
    pub(crate) fn read(text: &str) -> Self {
        let (config, notes) = amacs::config::AmacsConfig::from_aterm_toml(text);
        Self { config, notes }
    }

    /// The reader's notes as config notices (`config amacs.<key>: …`).
    pub(crate) fn notices(&self) -> Vec<String> {
        self.notes
            .iter()
            .map(|note| format!("config {note}"))
            .collect()
    }
}

/// Whether a tab gets the hint: `[amacs] default_editor` is on, `[packages] exclude` does not
/// name amacs, the session is not contained (a sandboxed shell may not reach the window that
/// would show the file), and `amacs` is a file in a directory of `path`, the `PATH` the tab is
/// handed.
pub(crate) fn wants_hint(
    on: bool,
    excluded: bool,
    contained: bool,
    path: Option<&std::ffi::OsStr>,
    is_file: impl Fn(&std::path::Path) -> bool,
) -> bool {
    if !on || excluded || contained {
        return false;
    }
    let Some(path) = path else {
        return false;
    };
    let name = format!("{EDITOR_WORD}{}", std::env::consts::EXE_SUFFIX);
    std::env::split_paths(path)
        .filter(|dir| dir.is_absolute())
        .any(|dir| is_file(&dir.join(&name)))
}

/// Put the hint in a factory's baseline environment `env_add`, or take it out, for `config`
/// and a session `contained` or not: at launch and at every config reload, so a Settings
/// change reaches the next tab. The `PATH` judged is the one `env_add` hands a tab, else the
/// one this process has.
pub(crate) fn settle(env_add: &mut Vec<(String, String)>, config: &Config, contained: bool) {
    let excluded = config
        .packages
        .as_ref()
        .and_then(|p| p.exclude.as_ref())
        .is_some_and(|names| names.iter().any(|name| name.trim() == EDITOR_WORD));
    let inherited = std::env::var_os("PATH");
    let path = env_add
        .iter()
        .rev()
        .find(|(key, _)| key == "PATH")
        .map(|(_, value)| std::ffi::OsString::from(value))
        .or(inherited);
    let wanted = wants_hint(
        config.amacs.config.default_editor,
        excluded,
        contained,
        path.as_deref(),
        std::path::Path::is_file,
    );
    place_hint(env_add, wanted);
}

/// [`settle`]'s edit of `env_add`: exactly one hint when `wanted`, none otherwise.
pub(crate) fn place_hint(env_add: &mut Vec<(String, String)>, wanted: bool) {
    env_add.retain(|(key, _)| key != ENV_DEFAULT_EDITOR);
    if wanted {
        env_add.push((ENV_DEFAULT_EDITOR.to_string(), EDITOR_WORD.to_string()));
    }
}

/// Where a new session's default `EDITOR` is decided (docs/DESIGN-amacs-2026-10-19.md §8.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Decides {
    /// In the shell's integration script, after the person's startup files: the hint stays.
    Script,
    /// Here, at spawn, over what the child inherits: nothing of the person's runs before the
    /// program (a `-e` command; cmd.exe, whose settings are the environment aterm hands on).
    Spawn,
    /// Nowhere: an interactive shell no script of aterm's runs in reads its startup files after
    /// anything set here, so a `VISUAL` set there could not stop it. The hint goes, and
    /// `EDITOR` is left as the session inherits it.
    Nowhere,
}

/// Where the default is decided for a session that `runs_command` (a non-empty `-e` argv) or
/// starts a shell whose integration aterm prepared (`integrated`, `None` when it has none:
/// an unknown shell, `--no-shell-integration`, a loader that could not be written).
pub(crate) fn decides(runs_command: bool, integrated: Option<ShellType>) -> Decides {
    if runs_command {
        return Decides::Spawn;
    }
    match integrated {
        Some(shell) if reads_default_editor_hint(shell) => Decides::Script,
        Some(ShellType::Cmd) => Decides::Spawn,
        _ => Decides::Nowhere,
    }
}

/// What the person set `key` (`EDITOR` or `VISUAL`) to in aterm's own environment, as `lookup`
/// reads it: an `EDITOR` an outer aterm's tab handed down is not theirs (the PTY spawn drops
/// it, [`is_handed_down_editor`]).
pub(crate) fn persons_own(
    key: &str,
    lookup: &dyn Fn(&str) -> Option<OsString>,
) -> Option<OsString> {
    let value = lookup(key)?;
    let handed_down =
        key == "EDITOR" && is_handed_down_editor(&value, lookup(ENV_ATERM_CHILD).as_deref());
    (!handed_down).then_some(value)
}

/// Settle the hint in a new session's `env_add` where [`decides`] says. [`Decides::Spawn`]
/// turns it into `EDITOR=amacs` when neither `EDITOR` nor `VISUAL` is set, non-empty, in what
/// the child gets: `env_add` itself, else the person's own in `lookup` (this process's
/// environment, [`persons_own`]); [`Decides::Nowhere`] drops it; [`Decides::Script`] leaves it
/// for the shell. Only a script reads the hint, so it leaves `env_add` in the other two.
pub(crate) fn settle_session(
    env_add: &mut Vec<(String, String)>,
    decides: Decides,
    lookup: &dyn Fn(&str) -> Option<OsString>,
) {
    if decides == Decides::Script {
        return;
    }
    let Some(at) = env_add
        .iter()
        .position(|(key, _)| key == ENV_DEFAULT_EDITOR)
    else {
        return;
    };
    let (_, word) = env_add.remove(at);
    env_add.retain(|(key, _)| key != ENV_DEFAULT_EDITOR);
    if decides == Decides::Nowhere {
        return;
    }
    let unset = |key: &str| {
        env_add
            .iter()
            .rev()
            .find(|(k, _)| k == key)
            .map(|(_, v)| OsString::from(v))
            .or_else(|| persons_own(key, lookup))
            .is_none_or(|v| v.is_empty())
    };
    if unset("EDITOR") && unset("VISUAL") {
        env_add.push(("EDITOR".to_string(), word));
    }
}

#[cfg(test)]
#[path = "default_editor_tests.rs"]
mod tests;
