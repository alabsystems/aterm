// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Programs BUILT INTO aterm (amacs design §8.5): commands the one `aterm` binary serves
//! under an argv0 alias beside itself. They update when aterm updates, and no index,
//! store build or shim ever holds them.
//!
//! # Apart from the stub roster, on purpose
//!
//! [`BUILTIN_PROGRAMS`] is not [`crate::stub::DEFAULT_SET_STUB_NAMES`], and
//! [`crate::stub::describe`] never consults it. The front door sends any name `describe`
//! knows to `atpkg run`, which would look for `amacs` in the store and find nothing; a
//! built-in name belongs to the front door's own verb ([`Builtin::verb`]). So nothing here
//! makes a name a roster program. What this module gives atpkg is the honest answer for
//! one:
//!
//! * `which` names the copy that runs: the alias beside the running aterm, in the state
//!   [`crate::state::built_in`], or a copy earlier on `PATH` that shadows it;
//! * `list` shows a "built in" section in its human form only (the porcelain rows are
//!   store builds, and pipes key on them byte for byte);
//! * `install`, `update` and `verify` say there is nothing to fetch or check, and exit 0;
//! * `uninstall` refuses and names the setting that turns the program's role off, and
//!   `pin`, `unpin`, `rollback` and `update --vendor|--alab` refuse: no store build exists
//!   to hold, go back to or trade.

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
    /// What to change instead of uninstalling, said to a person who asked to: a
    /// built-in program cannot leave the binary, but its role on this machine can end.
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
/// `None` when the executable's path cannot be read.
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
/// Reads the running executable's directory, this build's version and the first `tool`
/// on `path_var` ([`crate::vendor::first_on_path`]).
#[must_use]
pub fn which_line(tool: &str, path_var: Option<&OsStr>) -> Option<String> {
    let program = builtin(tool)?;
    Some(which_line_at(
        program,
        alias_dir().as_deref(),
        crate::vendor::first_on_path(program.name, path_var).as_deref(),
        aterm_types::version::app_version(),
    ))
}

/// The pure half of [`which_line`]. `first_on_path` is the copy a shell runs when the name
/// is typed (`None`: nothing on `PATH` answers to it), and the first arrow names what
/// runs, as on every `which` line:
///
/// * the alias, or nothing on `PATH` ⇒ `<name> → <dir>/<name> — built in: aterm <version>`;
/// * any other copy ⇒ `<name> → <copy> — built in: aterm <version> — SHADOWED by <copy>`.
///
/// When no live executable answers at the alias (an install from before it, or an
/// unreadable executable path, `dir` = `None`), the line ends `(not laid in this install)`,
/// naming the alias when another copy leads the line.
#[must_use]
pub fn which_line_at(
    program: &Builtin,
    dir: Option<&Path>,
    first_on_path: Option<&Path>,
    version: &str,
) -> String {
    let file = format!("{}{}", program.name, std::env::consts::EXE_SUFFIX);
    let (alias, laid) = match dir {
        // The same test every laid file's hand-over asks: an executable regular file, a
        // link to one counting (an alias is a link to `aterm`, or a hard link on Windows).
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

/// Whether `a` and `b` are one file: as spelled, or as both resolve. An alias is a link
/// to the `aterm` beside it, and a `~/.local/bin` link to the alias resolves there too.
fn same_file(a: &Path, b: &Path) -> bool {
    a == b
        || matches!(
            (std::fs::canonicalize(a), std::fs::canonicalize(b)),
            (Ok(x), Ok(y)) if x == y
        )
}

/// The "built in" section of `list`'s human form: a heading, then one aligned row per
/// program. Never part of the porcelain form.
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

/// What a store verb answers for a built-in program: the line and whether it is a
/// refusal (stderr, exit 1) or the answer (stdout, exit 0).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerbAnswer {
    pub line: String,
    pub refused: bool,
}

/// The answer `verb` gives when its operands (the argv after the verb), less the verb's
/// own `flags`, which the arity gate does not count as names either, are exactly one
/// built-in program's name. Else `None`, and the verb runs as it always did, its own
/// grammar refusing what it refuses (`install --default-set amacs` stays a usage error).
///
/// * `install` and `update` (`--retry` too) have nothing to fetch, and `verify` no store
///   build to check: the answer;
/// * `uninstall` cannot remove what is part of the binary, and names [`Builtin::opt_out`];
/// * `pin`, `unpin`, `rollback` and `update --vendor|--alab` refuse: there is no store
///   build to hold, go back to or trade for another.
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
mod tests {
    use super::*;

    /// THE ROSTER ARM STAYS AWAY. The front door sends any name [`crate::stub::describe`]
    /// knows to `atpkg run`, which looks in the store: a built-in program must be unknown
    /// to every compiled roster that feeds it, and answered by no reroute row (which
    /// `which` asks first) and no vendor lane.
    #[test]
    fn describe_does_not_know_amacs() {
        assert!(builtin("amacs").is_some(), "amacs is built in");
        for program in BUILTIN_PROGRAMS {
            let name = program.name;
            assert_eq!(crate::stub::describe(name), None, "{name}");
            assert!(
                !crate::stub::DEFAULT_SET_STUB_NAMES
                    .iter()
                    .any(|(n, _)| *n == name),
                "{name} is in the stub roster"
            );
            assert!(!crate::stub::is_agent_program(name), "{name}");
            assert!(!crate::vendor_direct::is_vendor(name), "{name}");
            assert!(crate::reroute::row_for(name).is_none(), "{name}");
            assert_eq!(crate::stub::program_of(name), name, "{name}");
            assert!(crate::store::ToolName::new(name).is_some(), "{name}");
        }
    }

    /// The line names the copy that runs: the alias beside the running aterm, or another
    /// `amacs` ahead of it on `PATH`; and it says when the alias is not there.
    #[cfg(unix)]
    #[test]
    fn the_which_line_names_the_copy_that_runs() {
        use std::os::unix::fs::PermissionsExt as _;
        let amacs = builtin("amacs").unwrap();
        let dir = crate::testdir::dir("atpkg-builtin-which-");
        let alias = dir.join("amacs");
        let executable = |path: &Path| {
            std::fs::write(path, b"#!/bin/true\n").unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        };
        let foreign_dir = crate::testdir::dir("atpkg-builtin-foreign-");
        let foreign = foreign_dir.join("amacs");
        executable(&foreign);

        let built_in = "built in: aterm 1.2.0";
        let not_laid = "(not laid in this install)";
        assert_eq!(
            which_line_at(amacs, Some(&dir), None, "1.2.0"),
            format!("amacs → {} — {built_in} {not_laid}", alias.display())
        );
        assert_eq!(
            which_line_at(amacs, Some(&dir), Some(&foreign), "1.2.0"),
            format!(
                "amacs → {f} — {built_in} — SHADOWED by {f} ({} is not laid in this install)",
                alias.display(),
                f = foreign.display()
            )
        );
        assert_eq!(
            which_line_at(amacs, None, None, "1.2.0"),
            format!("amacs → amacs — {built_in} {not_laid}")
        );

        // An alias is a link to the executable it names.
        executable(&dir.join("aterm"));
        std::os::unix::fs::symlink("aterm", &alias).unwrap();
        let ours = format!("amacs → {} — {built_in}", alias.display());
        assert_eq!(which_line_at(amacs, Some(&dir), None, "1.2.0"), ours);
        assert_eq!(
            which_line_at(amacs, Some(&dir), Some(&alias), "1.2.0"),
            ours
        );
        // A `~/.local/bin` link to the alias runs the same aterm: no shadow.
        let local_bin = crate::testdir::dir("atpkg-builtin-local-bin-");
        std::os::unix::fs::symlink(&alias, local_bin.join("amacs")).unwrap();
        assert_eq!(
            which_line_at(amacs, Some(&dir), Some(&local_bin.join("amacs")), "1.2.0"),
            ours
        );
        assert_eq!(
            which_line_at(amacs, Some(&dir), Some(&foreign), "1.2.0"),
            format!(
                "amacs → {f} — {built_in} — SHADOWED by {f}",
                f = foreign.display()
            )
        );

        // The copy that runs is the first on PATH, a relative entry never counting.
        let path = std::env::join_paths([Path::new("relative"), &foreign_dir, &local_bin]).unwrap();
        assert_eq!(
            crate::vendor::first_on_path("amacs", Some(&path)),
            Some(foreign.clone())
        );
        assert_eq!(which_line("ay", Some(&path)), None, "ay is not built in");
    }

    /// Only a lone name of a built-in program, the verb's own flags aside, is answered,
    /// by the verbs that would otherwise reach the store for it; any other argv is the
    /// verb's own to judge.
    #[test]
    fn verb_answer_takes_only_a_lone_built_in_operand() {
        let argv = |xs: &[&str]| xs.iter().map(ToString::to_string).collect::<Vec<_>>();
        let update_flags = ["--retry", "--vendor", "--alab"];
        let answer = |verb: &str, xs: &[&str], flags: &[&str]| {
            verb_answer(verb, &argv(xs), flags, "1.2.0").map(|a| a.refused)
        };
        assert_eq!(
            verb_answer("install", &argv(&["amacs"]), &[], "1.2.0"),
            Some(VerbAnswer {
                line: "atpkg: amacs is built into aterm 1.2.0; nothing to install".into(),
                refused: false,
            })
        );
        for (verb, xs, flags, refused) in [
            ("update", &["amacs"][..], &update_flags[..], false),
            ("update", &["amacs", "--retry"], &update_flags, false),
            ("update", &["--retry", "amacs"], &update_flags, false),
            ("update", &["amacs", "--vendor"], &update_flags, true),
            ("update", &["--alab", "amacs"], &update_flags, true),
            ("verify", &["amacs"], &[], false),
            ("uninstall", &["amacs"], &[], true),
            ("pin", &["amacs"], &[], true),
            ("unpin", &["amacs"], &[], true),
            ("rollback", &["amacs"], &[], true),
        ] {
            assert_eq!(answer(verb, xs, flags), Some(refused), "{verb} {xs:?}");
        }
        for (verb, xs, flags) in [
            ("install", &["ay"][..], &[][..]),
            ("install", &["amacs", "ay"], &[]),
            ("install", &["--default-set", "amacs"], &[]),
            ("install", &[], &[]),
            ("update", &["--retry"], &update_flags),
            ("update", &[], &update_flags),
            ("link", &["amacs"], &[]),
        ] {
            assert_eq!(answer(verb, xs, flags), None, "{verb} {xs:?}");
        }
    }
}
