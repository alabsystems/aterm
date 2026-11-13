// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn ends_with_exe(program: &str, exe: &str) -> bool {
    program.to_ascii_lowercase().ends_with(exe)
}

#[test]
fn config_shell_reads_the_top_level_key_the_window_reads() {
    assert_eq!(config_shell_from_text(""), ConfigShell::Absent);
    assert_eq!(
        config_shell_from_text("# shell = \"bash\"\nfont_px = 14\n"),
        ConfigShell::Absent,
        "the starter file's commented example is not a setting"
    );
    assert_eq!(
        config_shell_from_text("[privacy]\nshell = \"bash\"\n"),
        ConfigShell::Absent,
        "a `shell` inside a table is not the top-level key"
    );
    assert_eq!(
        config_shell_from_text("font_px = 14\nshell = \"bash\"\n"),
        ConfigShell::Set("bash".to_string())
    );
    assert_eq!(
        config_shell_from_text("shell = 'C:\\Program Files\\Git\\bin\\bash.exe'\n"),
        ConfigShell::Set("C:\\Program Files\\Git\\bin\\bash.exe".to_string()),
        "a literal string keeps its backslashes"
    );
    assert_eq!(
        config_shell_from_text("shell = \"\"\n"),
        ConfigShell::Absent,
        "an empty shell is unset — select_shell filters it the same way"
    );
    assert!(matches!(
        config_shell_from_text("shell = 5\n"),
        ConfigShell::Unusable(why) if why.contains("not a string")
    ));
    assert!(
        matches!(
            config_shell_from_text("shell = \"a\"\nshell = \"b\"\n"),
            ConfigShell::Unusable(why) if why.contains("does not parse")
        ),
        "a duplicate key is refused, as the window refuses it"
    );
    assert!(matches!(
        config_shell_from_text("shell = [[\n"),
        ConfigShell::Unusable(why) if why.contains("does not parse")
    ));
}

#[test]
fn window_shell_follows_the_windows_precedence_and_names_its_source() {
    let config = WindowShell::resolve_with(ConfigShell::Set("cmd".to_string()));
    assert!(
        ends_with_exe(&config.program, "cmd.exe"),
        "{}",
        config.program
    );
    assert_eq!(config.source, ShellSource::Config("cmd".to_string()));
    assert_eq!(config.origin_token(), "aterm.toml");
    assert_eq!(
        config.origin_sentence(),
        "a new window's default shell, from aterm.toml shell = \"cmd\""
    );

    let missing =
        WindowShell::resolve_with(ConfigShell::Set("aterm-no-such-shell-xyz".to_string()));
    assert_eq!(
        missing.program, "aterm-no-such-shell-xyz",
        "an unresolved name is reported verbatim, as CreateProcessW receives it"
    );
    assert_eq!(
        shell_check(&missing.program),
        crate::ShellCheck::Missing,
        "…and the shell check then fails, so doctor says FAIL"
    );

    let unusable =
        WindowShell::resolve_with(ConfigShell::Unusable("does not parse: x".to_string()));
    assert!(
        matches!(
            unusable.source,
            ShellSource::Default(_, ConfigShell::Unusable(_))
        ),
        "{:?}",
        unusable.source
    );
    assert!(
        unusable
            .origin_sentence()
            .contains("aterm.toml is not usable (does not parse: x)"),
        "{}",
        unusable.origin_sentence()
    );
}

#[test]
fn window_shell_default_is_the_spawns_default() {
    let dflt = WindowShell::resolve_with(ConfigShell::Absent);
    let (program, origin) = aterm_pty::select_shell_with_origin(None);
    assert_eq!(dflt.program, program.to_string_lossy());
    assert!(matches!(dflt.source, ShellSource::Default(o, ConfigShell::Absent) if o == origin));
    assert!(
        dflt.origin_token().starts_with("default:"),
        "{}",
        dflt.origin_token()
    );
    assert!(
        dflt.origin_sentence()
            .starts_with("a new window's default shell, the platform default ("),
        "{}",
        dflt.origin_sentence()
    );
    if matches!(
        aterm_pty::classify_shell_name(std::ffi::OsStr::new("pwsh")),
        aterm_pty::ShellResolution::Resolved(_)
    ) {
        assert_eq!(dflt.origin_token(), "default:pwsh");
        assert!(ends_with_exe(&dflt.program, "pwsh.exe"), "{}", dflt.program);
        assert_eq!(shell_check(&dflt.program), crate::ShellCheck::Runs);
    }
}

#[test]
fn inside_a_tab_the_report_is_the_tabs_own_shell() {
    let cmd = r"C:\Windows\system32\cmd.exe".to_string();
    let tab = WindowShell::from_tab(Some(cmd.clone())).expect("a tab said its shell");
    assert_eq!(tab.program, cmd);
    assert_eq!(tab.source, ShellSource::Tab);
    assert_eq!(tab.origin_token(), "tab");
    assert_eq!(tab.origin_sentence(), "this tab's shell");
    assert!(WindowShell::from_tab(None).is_none(), "outside a tab");
    assert!(
        WindowShell::from_tab(Some(String::new())).is_none(),
        "an empty value says nothing"
    );
}
