// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// A twin carrying the token block (DESIGN-claude-setup-token S8) still names its build: the
/// block sits before the last line, the plain exec, and names the build only inside it.
#[cfg(unix)]
#[test]
fn a_twin_with_the_token_block_still_names_its_build() {
    let body = "#!/bin/sh\nexport A=1\nexec '/s/store/claude/7/bin/claude' \"$@\"\n";
    let with = atpkg::launch_tap::with_token_block(
        "claude",
        body.to_owned(),
        Some(Path::new("/A/MacOS/atpkg")),
    );
    assert!(with.contains("token-run --auto --"), "{with}");
    assert_eq!(
        twin_binary(&with),
        Some(std::path::PathBuf::from("/s/store/claude/7/bin/claude"))
    );
}

#[test]
fn a_twin_names_its_binary_in_its_hosts_dialects_and_only_absolutely() {
    let sh = |p: &str| format!("#!/bin/sh\nexec '{p}' \"$@\"\n");
    let cmd = |p: &str| format!("@echo off\r\n@\"{p}\" %* & @exit /b\r\n");
    let named = |p: &str| Some(std::path::PathBuf::from(p));
    assert_eq!(twin_binary(&sh("/s/codex")), named("/s/codex"));
    // A `.cmd` twin is Windows' alone: elsewhere its body names nothing, even an absolute path.
    let windows = if cfg!(windows) {
        r"C:\s\codex.exe"
    } else {
        "/s/codex"
    };
    let on_windows = |p: &str| named(p).filter(|_| cfg!(windows));
    assert_eq!(twin_binary(&cmd(windows)), on_windows(windows));
    assert_eq!(
        twin_binary(&format!("@\"{windows}\" %*\r\n")),
        on_windows(windows),
        "the legacy tail"
    );
    for relative in ["s/codex", r"s\codex.exe"] {
        assert_eq!(twin_binary(&sh(relative)), None, "{relative}");
        assert_eq!(twin_binary(&cmd(relative)), None, "{relative}");
    }
    assert_eq!(
        twin_binary(&sh(r"C:\s\codex.exe")),
        None,
        "a sh twin names a `/` path"
    );
    assert_eq!(twin_binary(&format!("@\"{windows}\" %* & echo\r\n")), None);
}

#[test]
fn the_twin_names_its_binary_on_its_last_exec_line() {
    let twin = "#!/bin/sh\ncase \"$1\" in\n  update) exec '/x/atpkg' __selfupdate;;\nesac\nexport DISABLE_AUTOUPDATER='1'\nexec '/P/store/claude/1000002000001000282/bin/claude' \"$@\"\n";
    assert_eq!(
        twin_binary(twin),
        Some(std::path::PathBuf::from(
            "/P/store/claude/1000002000001000282/bin/claude"
        ))
    );
    assert_eq!(twin_binary("exec 'relative/claude' \"$@\""), None);
    assert_eq!(twin_binary("#!/bin/sh\n"), None);
    for other in [
        "#!/bin/bash\nexec '/P/store/claude/b/bin/claude' \"$@\"\n",
        "#!/bin/sh/not-an-interpreter\nexec '/P/store/claude/b/bin/claude' \"$@\"\n",
        "#!/bin/sh\r\nexec '/P/store/claude/b/bin/claude' \"$@\"\n",
        "#!/bin/sh -e\nexec '/P/store/claude/b/bin/claude' \"$@\"\n",
        "#!/bin/sh\nexec 'relative/claude' \"$@\"\n",
        "#!/bin/sh\nexec '/a'\"'\"'b/claude' \"$@\"\n",
        "#!/bin/sh\nexec '/P/store/claude/b/bin/claude' \"$@\"\necho after\n",
        "#!/bin/sh\nexec /P/store/claude/b/bin/claude \"$@\"\n",
    ] {
        assert_eq!(twin_binary(other), None, "{other}");
    }
}
