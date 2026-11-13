// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#[test]
fn a_shell_tab_is_told_its_shell_and_a_command_is_told_nothing() {
    let (key, cmd) = super::tab_shell_env(false, Some("cmd")).expect("a shell tab");
    assert_eq!(key, "ATERM_TAB_SHELL");
    assert!(
        std::path::Path::new(&cmd).is_absolute() && cmd.to_ascii_lowercase().ends_with("cmd.exe"),
        "{cmd}"
    );
    let (_, default) = super::tab_shell_env(false, None).expect("a default-shell tab");
    let (program, _) = aterm_pty::select_shell_with_origin(None);
    assert_eq!(default, program.to_string_lossy());
    assert_eq!(super::tab_shell_env(true, Some("cmd")), None);
    assert!(
        aterm_types::domain::is_ai_env_var(&key),
        "deny-listed at the hop"
    );
}
