// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

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
