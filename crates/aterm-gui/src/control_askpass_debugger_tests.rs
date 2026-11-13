// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::sudo_target_is_debugger;

fn classify(argv: &[&str]) -> bool {
    let argv = argv
        .iter()
        .map(|arg| (*arg).to_string())
        .collect::<Vec<_>>();
    sudo_target_is_debugger(Some(&argv)).expect("complete, supported exact argv")
}

/// Recognizes a root debugger past `sudo` flags and environment assignments only when it is the target command.
#[test]
fn debugger_target_is_recognized_only_as_the_command() {
    let cases: &[(&str, &[&str], bool)] = &[
        ("bare debugger", &["sudo", "lldb", "./a.out"], true),
        (
            "after -u and a path",
            &["sudo", "-u", "root", "/usr/bin/debugserver", "-", "./a.out"],
            true,
        ),
        (
            "absolute sudo, flags",
            &["/usr/bin/sudo", "-A", "-k", "gdb"],
            true,
        ),
        ("dtrace", &["sudo", "dtrace", "-n", "syscall:::"], true),
        (
            "after --user's value",
            &["sudo", "--user", "root", "lldb"],
            true,
        ),
        (
            "after --prompt's value",
            &["sudo", "--prompt", "Password:", "gdb"],
            true,
        ),
        (
            "after clustered -Anu's value",
            &["sudo", "-Anu", "root", "/usr/bin/debugserver"],
            true,
        ),
        ("after an assignment", &["sudo", "FOO=bar", "lldb"], true),
        (
            "prompt value holding a space",
            &["sudo", "--prompt", "hello make", "lldb"],
            true,
        ),
        (
            "whitespace inside the path",
            &["sudo", "/tmp/private tools/lldb", "./a.out"],
            true,
        ),
        (
            "after --user= and --",
            &["sudo", "--user=root", "--", "lldb"],
            true,
        ),
        ("after attached -uroot", &["sudo", "-uroot", "lldb"], true),
        ("path argument", &["sudo", "rm", "-rf", "/tmp/lldb"], false),
        (
            "package argument",
            &["sudo", "apt", "install", "gdb"],
            false,
        ),
        ("ordinary command", &["sudo", "make", "install"], false),
        (
            "argument after --user's value",
            &["sudo", "--user", "root", "make", "lldb"],
            false,
        ),
        (
            "assignment value",
            &["sudo", "FOO=lldb", "make", "install"],
            false,
        ),
        (
            "same text, split prompt",
            &["sudo", "--prompt", "hello", "make", "lldb"],
            false,
        ),
        (
            "argument after a spaced path",
            &["sudo", "/tmp/private tools/make", "lldb"],
            false,
        ),
        ("option after --", &["sudo", "--", "--user", "lldb"], false),
        (
            "argument after --preserve-env=",
            &["sudo", "--preserve-env=PATH,HOME", "make", "lldb"],
            false,
        ),
    ];
    let wrong = cases
        .iter()
        .filter(|(_, argv, debugger)| classify(argv) != *debugger)
        .map(|(case, argv, debugger)| format!("{case}: {argv:?} should be {debugger}"))
        .collect::<Vec<_>>();
    assert!(wrong.is_empty(), "misclassified: {wrong:#?}");
}

#[test]
fn absent_or_unclassifiable_argv_never_admits_a_sheet() {
    assert!(sudo_target_is_debugger(None).is_err());
    for argv in [
        vec![],
        vec!["(sudo)".into()],
        vec!["sudo".into(), "--user".into()],
        vec!["sudo".into(), "--new-option".into(), "lldb".into()],
        vec!["sudo".into(), "-Z".into(), "make".into()],
        vec!["sudo".into(), "FOO=bar".into()],
        vec!["sudo".into(), "odd/name=value".into(), "lldb".into()],
        vec!["sudo".into(), "=value".into(), "lldb".into()],
    ] {
        assert!(sudo_target_is_debugger(Some(&argv)).is_err(), "{argv:?}");
    }
}
