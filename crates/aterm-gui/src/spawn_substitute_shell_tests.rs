// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{shell_to_start, substitute_shell};

#[test]
fn only_a_shell_that_cannot_start_is_replaced_and_the_pane_says_so() {
    assert_eq!(substitute_shell(Some("/bin/sh"), None), None);
    assert_eq!(substitute_shell(Some("sh"), None), None, "found on PATH");
    let sub = substitute_shell(Some("/nonexistent/aterm-test-zsh"), Some(&[]))
        .expect("a missing shell is replaced");
    assert!(
        aterm_pty::shell_cannot_start(
            std::ffi::OsStr::new(&sub.program),
            aterm_pty::ShellOrigin::Override
        )
        .is_none(),
        "the substitute can start: {sub:?}"
    );
    let said = "[aterm: `/nonexistent/aterm-test-zsh` does not exist (set by --shell or \
                `shell` in aterm.toml); started ";
    assert!(
        sub.notice.starts_with(said)
            && sub.notice.contains(&format!("started {}, ", sub.program))
            && sub.notice.ends_with(" instead]"),
        "{sub:?}"
    );
    assert_eq!(sub.shell_env, None, "$SHELL was not what broke");
}

#[test]
fn a_substitute_starts_without_the_broken_shells_args() {
    let args = ["--no-config".to_string()];
    let broken = Some("/nonexistent/aterm-test-fish");
    let sub = substitute_shell(broken, Some(&args)).expect("a missing shell is replaced");
    assert!(
        sub.notice.ends_with(" instead, without `shell_args`]"),
        "{sub:?}"
    );
    assert_eq!(
        shell_to_start(Some(&sub), broken, Some(&args)),
        (Some(sub.program.as_str()), None)
    );
    assert_eq!(
        shell_to_start(None, Some("/bin/sh"), Some(&args)),
        (Some("/bin/sh"), Some(&args[..]))
    );
}
