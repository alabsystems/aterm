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
        aterm_types::version::app_version(),
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
mod tests {
    use super::*;

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

        executable(&dir.join("aterm"));
        std::os::unix::fs::symlink("aterm", &alias).unwrap();
        let ours = format!("amacs → {} — {built_in}", alias.display());
        assert_eq!(which_line_at(amacs, Some(&dir), None, "1.2.0"), ours);
        assert_eq!(
            which_line_at(amacs, Some(&dir), Some(&alias), "1.2.0"),
            ours
        );
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

        let path = std::env::join_paths([Path::new("relative"), &foreign_dir, &local_bin]).unwrap();
        assert_eq!(
            crate::vendor::first_on_path("amacs", Some(&path)),
            Some(foreign.clone())
        );
        assert_eq!(which_line("ay", Some(&path)), None, "ay is not built in");
    }

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
