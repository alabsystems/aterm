// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Programs built into aterm.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// One program built into aterm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Builtin {
    /// The command name: the argv0 alias of `aterm` that runs it.
    pub name: &'static str,
    /// The front-door verb it is (`aterm <verb>`).
    pub verb: &'static str,
    /// The authored one-line what-it-is, as `list` shows it.
    pub summary: &'static str,
    /// What to change instead of uninstalling, said to a person who asked to: a built-in program
    /// cannot leave the binary, but its role on this machine can end.
    pub opt_out: &'static str,
}

/// The programs built into aterm, in the order `list` shows them.
pub const BUILTIN_PROGRAMS: &[Builtin] = &[Builtin {
    name: "amacs",
    verb: "edit",
    summary: "aterm's editor",
    opt_out: "set `[amacs] default_editor = false` in aterm.toml to stop it being the \
              default editor",
}];

/// The built-in program named `name`, if there is one.
#[must_use]
pub fn builtin(name: &str) -> Option<&'static Builtin> {
    BUILTIN_PROGRAMS.iter().find(|b| b.name == name)
}

/// The directory the running aterm's argv0 aliases are laid in: its own executable's,
/// links resolved, which is the rule [`crate::stub::co_located_atpkg_path`] finds the
/// `atpkg` alias by. An alias found through `~/.local/bin` resolves to the bundle first.
#[must_use]
pub fn alias_dir() -> Option<PathBuf> {
    let dir = std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .ok()?
        .parent()?
        .to_path_buf();
    Some(if cfg!(windows) {
        crate::platform::strip_verbatim_prefix(&dir)
    } else {
        dir
    })
}

/// `which`'s answer for `tool` when it is built in, else `None` (the store's arms answer).
#[must_use]
pub fn which_line(tool: &str, path_var: Option<&OsStr>) -> Option<String> {
    let program = builtin(tool)?;
    Some(which_line_at(
        program,
        alias_dir().as_deref(),
        crate::vendor::first_on_path(program.name, path_var).as_deref(),
        aterm_stamp::app_version(),
    ))
}

/// The pure half of [`which_line`].
#[must_use]
pub fn which_line_at(
    program: &Builtin,
    dir: Option<&Path>,
    first_on_path: Option<&Path>,
    version: &str,
) -> String {
    let file = format!("{}{}", program.name, std::env::consts::EXE_SUFFIX);
    let (alias, laid) = match dir {
        Some(dir) => {
            let path = dir.join(&file);
            let laid = crate::stub::is_live_atpkg(&path);
            (path, laid)
        }
        None => (PathBuf::from(&file), false),
    };
    let shadow = first_on_path.filter(|copy| !(laid && same_file(copy, &alias)));
    let name = program.name;
    match (shadow, laid) {
        (None, true) => format!(
            "{name} → {} — {}",
            alias.display(),
            crate::state::built_in(version)
        ),
        (None, false) => format!(
            "{name} → {} — {} (not laid in this install)",
            alias.display(),
            crate::state::built_in(version)
        ),
        (Some(copy), true) => format!(
            "{name} → {} — {}",
            copy.display(),
            crate::state::built_in_shadowed(version, copy)
        ),
        (Some(copy), false) => format!(
            "{name} → {} — {} ({} is not laid in this install)",
            copy.display(),
            crate::state::built_in_shadowed(version, copy),
            alias.display()
        ),
    }
}

fn same_file(a: &Path, b: &Path) -> bool {
    a == b
        || matches!(
            (std::fs::canonicalize(a), std::fs::canonicalize(b)),
            (Ok(x), Ok(y)) if x == y
        )
}

/// The "built in" section of `list`'s human form: a heading, then one aligned row per program.
#[must_use]
pub fn list_human_lines(version: &str) -> Vec<String> {
    let width = BUILTIN_PROGRAMS
        .iter()
        .map(|b| b.name.len())
        .max()
        .unwrap_or(0);
    let mut lines = vec![format!(
        "atpkg: built into aterm {version} — updated with aterm, never from the store"
    )];
    lines.extend(
        BUILTIN_PROGRAMS
            .iter()
            .map(|b| format!("  {:<width$}  {}", b.name, b.summary)),
    );
    lines
}

/// What a store verb answers for a built-in program.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerbAnswer {
    pub line: String,
    pub refused: bool,
}

/// The answer `verb` gives when its operands (the argv after the verb), less the verb's own
/// `flags`, which the arity gate does not count as names either, are exactly one built-in program's
/// name.
#[must_use]
pub fn verb_answer(
    verb: &str,
    operands: &[String],
    flags: &[&str],
    version: &str,
) -> Option<VerbAnswer> {
    let (names, given): (Vec<&String>, Vec<&String>) =
        operands.iter().partition(|a| !flags.contains(&a.as_str()));
    let [name] = names[..] else {
        return None;
    };
    let program = builtin(name)?;
    let head = format!("atpkg: {} is built into aterm {version}", program.name);
    let source_choice = given.iter().any(|f| {
        f.as_str() == crate::cli::SOURCE_VENDOR_FLAG || f.as_str() == crate::cli::SOURCE_ALAB_FLAG
    });
    let (line, refused) = match verb {
        "install" => (format!("{head}; nothing to install"), false),
        "update" if source_choice => (
            format!("{head}; it has no other build to choose — it updates with aterm"),
            true,
        ),
        "update" => (format!("{head}; it updates with aterm"), false),
        "verify" => (
            format!("{head}; the store holds no build of it to verify"),
            false,
        ),
        "uninstall" => (
            format!("{head} and cannot be uninstalled — {}", program.opt_out),
            true,
        ),
        "pin" | "unpin" | "rollback" => {
            let act = if verb == "rollback" {
                "roll back"
            } else {
                verb
            };
            (
                format!("{head}; it has no store build to {act} — it updates with aterm"),
                true,
            )
        }
        _ => return None,
    };
    Some(VerbAnswer { line, refused })
}

#[cfg(test)]
#[path = "builtin_tests.rs"]
mod tests;
