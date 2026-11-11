// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::os::unix::fs::PermissionsExt as _;

struct Bed {
    _root: aterm_tempfile::TempDir,
    bin: PathBuf,
    path: OsString,
}

impl Bed {
    fn new(tag: &str) -> Self {
        let root = aterm_tempfile::Builder::new()
            .prefix(&format!("aterm-pty-shell-{tag}-"))
            .tempdir()
            .expect("an exclusively owned scratch root");
        let bin = root.path().join("bin");
        std::fs::create_dir_all(&bin).expect("a scratch bin");
        for (name, mode) in [("fish", 0o755), ("notes", 0o644)] {
            let file = bin.join(name);
            std::fs::write(&file, b"#!/bin/sh\n").expect("a scratch file");
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(mode))
                .expect("its mode");
        }
        let mut path = OsString::from(":");
        path.push(root.path().join("missing"));
        path.push(":");
        path.push(&bin);
        Self {
            _root: root,
            bin,
            path,
        }
    }
}

fn os(s: &str) -> OsString {
    OsString::from(s)
}

#[test]
fn the_shell_is_the_override_then_shell_then_the_account_then_sh() {
    let bed = Bed::new("order");
    let path = Some(bed.path.as_os_str());
    let fish = bed.bin.join("fish").into_os_string();
    let account = || Some(os("/bin/bash"));
    assert_eq!(
        select_shell_in(
            Some(OsStr::new("fish")),
            Some(os("/bin/zsh")),
            path,
            account
        ),
        (fish.clone(), ShellOrigin::Override),
        "a bare override is looked up on PATH"
    );
    assert_eq!(
        select_shell_in(Some(OsStr::new("")), Some(os("fish")), path, account),
        (fish, ShellOrigin::Env),
        "an empty override is none, and a bare $SHELL is looked up too"
    );
    assert_eq!(
        select_shell_in(None, Some(os("/bin/zsh")), path, account),
        (os("/bin/zsh"), ShellOrigin::Env),
        "a path is taken as it is"
    );
    for env in [None, Some(os(""))] {
        assert_eq!(
            select_shell_in(None, env.clone(), path, account),
            (os("/bin/bash"), ShellOrigin::Passwd),
            "$SHELL {env:?} names nothing: the account's login shell"
        );
        assert_eq!(
            select_shell_in(None, env, path, || None),
            (os("/bin/sh"), ShellOrigin::Default)
        );
    }
}

#[test]
fn a_name_path_cannot_resolve_stays_as_typed() {
    let bed = Bed::new("typed");
    let path = Some(bed.path.as_os_str());
    for name in ["zsh-nowhere", "notes"] {
        assert_eq!(
            select_shell_in(Some(OsStr::new(name)), None, path, || None).0,
            os(name)
        );
    }
    assert_eq!(on_path(OsStr::new("fi\0sh"), path), os("fi\0sh"));
    assert_eq!(on_path(OsStr::new("fish"), None), os("fish"), "no PATH");
    assert_eq!(find_on_path(OsStr::new(""), path), None);
    assert_eq!(find_on_path(OsStr::new("./fish"), path), None);
}

#[test]
fn a_relative_path_entry_is_not_searched() {
    let bed = Bed::new("relative");
    let cwd = std::env::current_dir().expect("the test's folder");
    let mut relative = PathBuf::new();
    for _ in cwd.components().skip(1) {
        relative.push("..");
    }
    relative.push(bed.bin.strip_prefix("/").expect("an absolute bin"));
    assert!(
        runnable(&relative.join("fish")),
        "the relative spelling names the program"
    );
    assert_eq!(
        find_on_path(OsStr::new("fish"), Some(relative.as_os_str())),
        None
    );
    assert_eq!(
        find_on_path(OsStr::new("fish"), Some(bed.bin.as_os_str())),
        Some(bed.bin.join("fish"))
    );
}

#[test]
fn a_shell_that_cannot_start_is_named_with_where_it_was_set() {
    let bed = Bed::new("cannot");
    let fish = bed.bin.join("fish");
    assert_eq!(
        shell_cannot_start(fish.as_os_str(), ShellOrigin::Override),
        None
    );
    assert_eq!(
        shell_cannot_start(OsStr::new("bash-nowhere"), ShellOrigin::Override).as_deref(),
        Some("`bash-nowhere` is not on PATH (set by --shell or `shell` in aterm.toml)")
    );
    let gone = bed.bin.join("zhs");
    assert_eq!(
        shell_cannot_start(gone.as_os_str(), ShellOrigin::Env),
        Some(format!("`{}` does not exist (from $SHELL)", gone.display()))
    );
    let notes = bed.bin.join("notes");
    assert_eq!(
        shell_cannot_start(notes.as_os_str(), ShellOrigin::Passwd),
        Some(format!(
            "`{}` is not executable (your login shell in the password database)",
            notes.display()
        ))
    );
    assert!(
        shell_cannot_start(OsStr::new("/bin/fi\0sh"), ShellOrigin::Override)
            .is_some_and(|why| why.contains("NUL")),
    );
}

#[test]
fn the_fallback_is_the_account_shell_then_sh_never_the_failed_one() {
    let bed = Bed::new("fallback");
    let fish = bed.bin.join("fish").into_os_string();
    let notes = bed.bin.join("notes").into_os_string();
    let failed = OsStr::new("/nonexistent/zsh");
    assert_eq!(
        fallback_shell_from(failed, || Some(fish.clone())),
        Some((fish.clone(), ShellOrigin::Passwd))
    );
    assert_eq!(
        fallback_shell_from(&fish, || Some(fish.clone())),
        Some((os("/bin/sh"), ShellOrigin::Default)),
        "the account's shell is the one that failed"
    );
    assert_eq!(
        fallback_shell_from(failed, || Some(notes.clone())),
        Some((os("/bin/sh"), ShellOrigin::Default)),
        "nobody may run the account's shell"
    );
    assert_eq!(
        fallback_shell_from(failed, || None),
        Some((os("/bin/sh"), ShellOrigin::Default))
    );
    assert_eq!(fallback_shell_from(OsStr::new("/bin/sh"), || None), None);
}

#[test]
fn the_account_shell_is_an_absolute_program_or_none() {
    if let Some(shell) = account_shell() {
        let path = Path::new(&shell);
        assert!(path.is_absolute() && runnable(path), "{shell:?}");
    }
    let (shell, origin) = login_shell();
    assert!(matches!(origin, ShellOrigin::Passwd | ShellOrigin::Default));
    assert!(Path::new(&shell).is_absolute(), "{shell:?}");
}
