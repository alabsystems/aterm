// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_string()).collect()
}

fn bash() -> InjectionEnv {
    InjectionEnv {
        env_add: vec![("ATERM_SHELL_INTEGRATION_DIR".to_string(), "/si".to_string())],
        argv_override: Some(strings(&["bash", "--rcfile", "/si/bash/x"])),
    }
}

fn no_env(_: &str) -> Option<String> {
    None
}

fn merged(args: &[&str], env: &dyn Fn(&str) -> Option<String>) -> (ArgsMerge, InjectionEnv) {
    let mut injection = bash();
    let merge = merge_shell_args_in(ShellType::Bash, &mut injection, &strings(args), env);
    (merge, injection)
}

fn argv(injection: &InjectionEnv) -> Vec<&str> {
    injection
        .argv_override
        .as_deref()
        .unwrap_or_default()
        .iter()
        .map(String::as_str)
        .collect()
}

#[cfg(all(unix, not(target_os = "macos")))]
fn added<'a>(injection: &'a InjectionEnv, key: &str) -> Option<&'a str> {
    injection
        .env_add
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
}

#[test]
fn bash_options_go_where_bash_accepts_them() {
    let (merge, injection) = merged(&["-o", "vi", "--verbose", "-O", "extglob", "-i"], &no_env);
    assert_eq!(merge, ArgsMerge::Integrated);
    assert_eq!(
        argv(&injection),
        [
            "bash",
            "--verbose",
            "--rcfile",
            "/si/bash/x",
            "-o",
            "vi",
            "-O",
            "extglob",
            "-i"
        ]
    );
    assert_eq!(injection.env_add.len(), 1, "no env of its own");
    let (_, empty) = merged(&[], &no_env);
    assert_eq!(argv(&empty), ["bash", "--rcfile", "/si/bash/x"], "no args");
    let (merge, injection) = merged(&["-noediting", "-i", "+o", "posix"], &no_env);
    assert_eq!(merge, ArgsMerge::Integrated);
    assert_eq!(
        argv(&injection),
        [
            "bash",
            "--noediting",
            "--rcfile",
            "/si/bash/x",
            "-i",
            "+o",
            "posix"
        ]
    );
}

#[test]
fn bash_args_that_are_no_interactive_shell_stand_integration_aside() {
    for (args, names) in [
        (&["-c", "echo"][..], "`-c`"),
        (&["-ic", "echo"], "`-c`"),
        (&["script.sh"], "`script.sh`"),
        (&["--posix"], "`--posix`"),
        (&["-o", "posix"], "`-o posix`"),
        (&["-io", "posix"], "`-o posix`"),
        (&["-posix"], "`-posix`"),
        (&["--verbose", "-restricted"], "`-restricted`"),
        (&["-help"], "`-help`"),
        (&["--restricted"], "`--restricted`"),
        (&["-r"], "`-r`"),
        (&["-s", "a"], "`-s`"),
        (&["--version"], "`--version`"),
        (&["--", "x"], "`--`"),
        (&["-o"], "`-o`"),
        (&["--rcfile"], "`--rcfile`"),
    ] {
        match merged(args, &no_env).0 {
            ArgsMerge::Bypass(why) => assert!(why.contains(names), "{args:?}: {why}"),
            ArgsMerge::Integrated => panic!("{args:?} kept integration"),
        }
    }
}

/// Off macOS a login bash, or `--norc`, starts in POSIX mode on the loader.
#[cfg(all(unix, not(target_os = "macos")))]
#[test]
fn a_login_bash_starts_in_posix_mode_on_the_loader() {
    let env = |key: &str| match key {
        "HOME" => Some("/home/u".to_string()),
        "ENV" => Some("/home/u/.shrc".to_string()),
        _ => None,
    };
    let (merge, injection) = merged(&["-l", "-i", "--noprofile"], &env);
    assert_eq!(merge, ArgsMerge::Integrated);
    assert_eq!(
        argv(&injection),
        ["bash", "--noprofile", "--posix", "-l", "-i"]
    );
    assert_eq!(added(&injection, "ATERM_BASH_POSIX"), Some("1"));
    assert_eq!(
        added(&injection, "ENV"),
        Some("${ATERM_SHELL_INTEGRATION_DIR}/bash/bashrc")
    );
    assert_eq!(added(&injection, "ATERM_BASH_ENV"), Some("/home/u/.shrc"));
    assert_eq!(added(&injection, "HISTFILE"), Some("/home/u/.bash_history"));
    assert_eq!(added(&injection, "ATERM_BASH_HISTFILE"), Some("1"));
    assert_eq!(added(&injection, "ATERM_BASH_NOPROFILE"), Some("1"));

    let (_, norc) = merged(&["--norc"], &|key| {
        (key == "HISTFILE").then(|| "/elsewhere".to_string())
    });
    assert_eq!(argv(&norc), ["bash", "--norc", "--posix"]);
    assert_eq!(added(&norc, "HISTFILE"), None, "theirs is inherited");
    assert_eq!(added(&norc, "ATERM_BASH_ENV"), None);
    assert_eq!(added(&norc, "ATERM_BASH_NOPROFILE"), None);

    let (_, login) = merged(&["--login"], &no_env);
    assert_eq!(argv(&login), ["bash", "--login", "--posix"]);

    let (_, rc) = merged(&["--rcfile", "/home/u/.myrc", "-x"], &no_env);
    assert_eq!(argv(&rc), ["bash", "--rcfile", "/si/bash/x", "-x"]);
    assert_eq!(added(&rc, "ATERM_BASH_RCFILE"), Some("/home/u/.myrc"));

    let (_, login) = merged(&["-login", "-i"], &no_env);
    assert_eq!(argv(&login), ["bash", "--login", "--posix", "-i"]);
    let (_, rc) = merged(&["-rcfile", "~/.myrc"], &no_env);
    assert_eq!(argv(&rc), ["bash", "--rcfile", "/si/bash/x"]);
    assert_eq!(added(&rc, "ATERM_BASH_RCFILE"), Some("~/.myrc"));
}

/// On macOS (and under Git Bash) the loader reads the login files itself.
#[cfg(not(all(unix, not(target_os = "macos"))))]
#[test]
fn the_login_style_loader_stands_in_for_login() {
    let (merge, injection) = merged(&["-li", "--login", "-o", "vi", "-l"], &no_env);
    assert_eq!(merge, ArgsMerge::Integrated);
    assert_eq!(
        argv(&injection),
        ["bash", "--rcfile", "/si/bash/x", "-i", "-o", "vi"]
    );
    for args in [&["--norc"][..], &["--noprofile"], &["--rcfile", "x"]] {
        assert!(
            matches!(merged(args, &no_env).0, ArgsMerge::Bypass(_)),
            "{args:?}"
        );
    }
}

/// Build a Git Bash login-start [`ShellInjection`] (`crate::BASH_LOGIN_START`).
#[cfg(windows)]
#[test]
fn git_bash_starts_as_a_login_shell_on_the_loader() {
    let mut injection =
        crate::injection_for(ShellType::Bash, std::path::Path::new(r"C:\si")).expect("bash");
    assert_eq!(argv(&injection), ["bash", "--login", "--posix"]);
    let has = |injection: &InjectionEnv, key: &str, value: &str| {
        injection
            .env_add
            .iter()
            .any(|(k, v)| k == key && v == value)
    };
    assert!(has(&injection, "ATERM_BASH_POSIX", "1"), "{injection:?}");
    assert!(
        has(
            &injection,
            "ENV",
            "${ATERM_SHELL_INTEGRATION_DIR}/bash/bashrc"
        ),
        "{injection:?}"
    );
    let mut env_add = Vec::new();
    posix_start_env(&mut env_add, false, &|key| {
        (key == "HOME").then(|| r"C:\Users\u".to_string())
    });
    assert!(
        env_add.contains(&("ATERM_BASH_HISTFILE".to_string(), "home".to_string()))
            && !env_add.iter().any(|(key, _)| key == "HISTFILE"),
        "a Windows HOME is no POSIX path: {env_add:?}"
    );
    let args = strings(&["-li", "--verbose", "-o", "vi"]);
    assert_eq!(
        merge_shell_args_in(ShellType::Bash, &mut injection, &args, &no_env),
        ArgsMerge::Integrated
    );
    assert_eq!(
        argv(&injection),
        ["bash", "--verbose", "--login", "--posix", "-i", "-o", "vi"]
    );
}

fn pwsh() -> InjectionEnv {
    InjectionEnv {
        env_add: Vec::new(),
        argv_override: Some(strings(&[
            "pwsh",
            "-ExecutionPolicy",
            "Bypass",
            "-NoExit",
            "-Command",
            ". x",
        ])),
    }
}

#[test]
fn powershell_parameters_go_first_unless_they_run_something_else() {
    let mut injection = pwsh();
    let args = strings(&["-Login", "-NoLogo", "-noe", "-WorkingDirectory", "/tmp"]);
    assert_eq!(
        merge_shell_args_in(ShellType::PowerShell, &mut injection, &args, &no_env),
        ArgsMerge::Integrated
    );
    assert_eq!(
        injection.argv_override.unwrap(),
        strings(&[
            "pwsh",
            "-Login",
            "-NoLogo",
            "-WorkingDirectory",
            "/tmp",
            "-ExecutionPolicy",
            "Bypass",
            "-NoExit",
            "-Command",
            ". x"
        ])
    );
    for arg in [
        "-c",
        "-Command",
        "-f",
        "-File:x.ps1",
        "-ec",
        "-e",
        "-ex",
        "-v",
        "-NonInteractive",
        "-?",
    ] {
        let mut injection = pwsh();
        assert!(
            matches!(
                merge_shell_args_in(
                    ShellType::PowerShell,
                    &mut injection,
                    &strings(&[arg]),
                    &no_env
                ),
                ArgsMerge::Bypass(_)
            ),
            "{arg}"
        );
    }
}

#[test]
fn the_config_check_answers_as_the_merge_does() {
    assert!(
        shell_args_bypass(ShellType::Bash, &strings(&["-c", "x"]))
            .is_some_and(|why| why.contains("`-c`"))
    );
    assert_eq!(
        shell_args_bypass(ShellType::Bash, &strings(&["-o", "vi"])),
        None
    );
    assert_eq!(
        shell_args_bypass(ShellType::Zsh, &strings(&["-c", "x"])),
        None
    );
    assert_eq!(
        shell_args_bypass(ShellType::Unknown, &strings(&["-c"])),
        None
    );
}

#[test]
fn shells_integrated_through_env_keep_their_argv_untouched() {
    for shell in [
        ShellType::Zsh,
        ShellType::Fish,
        ShellType::Wsl,
        ShellType::Cmd,
    ] {
        let mut injection = bash();
        assert_eq!(
            merge_shell_args_in(shell, &mut injection, &strings(&["-c", "x"]), &no_env),
            ArgsMerge::Integrated
        );
        assert_eq!(argv(&injection), ["bash", "--rcfile", "/si/bash/x"]);
    }
}
