// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Integration test: Safety mode with allowlist permits listed operations.
//!
//! Fresh OnceLock binary — initializes Safety mode + allowlist, then verifies
//! that listed MCP tools, plugins, network targets, and commands are allowed.
//! that listed MCP tools, plugins, network targets, and processes are allowed.

use aterm_containment::{AllowlistConfig, ContainmentMode};

#[test]
fn safety_mode_allows_listed_operations() {
    // Initialize Safety mode.
    aterm_containment::init_mode(ContainmentMode::Safety).expect("mode init");

    // `is_process_allowed` canonicalizes each rule and drops any that do not
    // resolve on disk — a deliberate hardening so only real executables can be
    // allowlisted. The list must therefore name binaries that exist on THIS host;
    // hardcoding /bin/zsh made the test fail wherever zsh is not installed. Probe
    // for the shells that are actually present instead. /bin/sh is POSIX-guaranteed,
    // so this is never empty.
    #[cfg(unix)]
    let shell_candidates: Vec<String> = ["/bin/sh", "/bin/bash", "/bin/zsh"]
        .into_iter()
        .map(String::from)
        .collect();
    // Windows twin: cmd.exe (via %ComSpec%, the Windows shell guarantee) plus
    // Windows PowerShell at its fixed System32 home when present.
    #[cfg(windows)]
    let shell_candidates: Vec<String> = vec![
        std::env::var("ComSpec").unwrap_or_else(|_| r"C:\Windows\System32\cmd.exe".to_string()),
        r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe".to_string(),
    ];
    let allowed_shells: Vec<String> = shell_candidates
        .into_iter()
        .filter(|p| std::path::Path::new(p).exists())
        .collect();
    assert!(
        !allowed_shells.is_empty(),
        "expected at least /bin/sh (POSIX) or %ComSpec% (Windows) to exist"
    );

    // Windows regression: a dotted bare name (`pyfake3.11`) must resolve via
    // PATH by APPENDING a PATHEXT extension (`pyfake3.11.exe`), not by
    // replacing the trailing dot segment (`pyfake3.exe`). Set up a fake
    // executable on PATH before the allowlist is frozen.
    #[cfg(windows)]
    // Tuple keeps the tempdir guard alive through the assertions below.
    let dotted_exe = {
        let dir = aterm_tempfile::tempdir().expect("tempdir");
        let exe = dir.path().join("pyfake3.11.exe");
        std::fs::write(&exe, "MZ").expect("write fake exe");
        let mut paths = vec![dir.path().to_path_buf()];
        paths.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ));
        let new_path = std::env::join_paths(paths).expect("join PATH");
        // SAFETY: single-test binary, no concurrent environment readers yet.
        unsafe { std::env::set_var("PATH", &new_path) };
        (dir, exe)
    };
    #[cfg(windows)]
    let mut processes = allowed_shells.clone();
    #[cfg(windows)]
    processes.push(
        dotted_exe
            .1
            .canonicalize()
            .expect("canonicalize fake exe")
            .display()
            .to_string(),
    );
    #[cfg(not(windows))]
    let processes = allowed_shells.clone();

    // Provide an allowlist with specific entries.
    let config = AllowlistConfig {
        mcp_tools: vec!["read_file".into(), "write_file".into()],
        plugins: vec!["spell-check".into()],
        network: vec!["localhost:*".into(), "unix:/tmp/aterm.sock".into()],
        processes,
    };
    aterm_containment::init_allowlist(config).expect("allowlist init");

    // Listed MCP tools are allowed.
    assert!(aterm_containment::is_mcp_allowed("read_file"));
    assert!(aterm_containment::is_mcp_allowed("write_file"));

    // Unlisted MCP tool is denied.
    assert!(!aterm_containment::is_mcp_allowed("execute_command"));

    // Listed plugin is allowed.
    assert!(aterm_containment::is_plugin_allowed("spell-check"));

    // Unlisted plugin is denied.
    assert!(!aterm_containment::is_plugin_allowed("evil-plugin"));

    // Listed network targets are allowed.
    assert!(aterm_containment::is_network_allowed("localhost:8080"));
    assert!(aterm_containment::is_network_allowed("localhost:443"));
    assert!(aterm_containment::is_network_allowed(
        "unix:/tmp/aterm.sock"
    ));

    // Unlisted network target is denied.
    assert!(!aterm_containment::is_network_allowed("example.com:80"));

    // Every listed shell that exists on the host is allowed.
    for shell in &allowed_shells {
        assert!(
            aterm_containment::is_process_allowed(shell),
            "listed shell {shell} should be allowed"
        );
    }

    // The dotted bare name resolves to `pyfake3.11.exe` on PATH (appended
    // PATHEXT extension), which is allowlisted.
    #[cfg(windows)]
    assert!(
        aterm_containment::is_process_allowed("pyfake3.11"),
        "dotted bare name must resolve by appending the PATHEXT extension"
    );

    // Unlisted paths fail closed: a path that resolves to nothing, and — more
    // tellingly — a real binary that was simply never listed.
    assert!(!aterm_containment::is_process_allowed("/bin/evil"));
    #[cfg(unix)]
    let unlisted_controls = ["/usr/bin/env", "/bin/cat", "/bin/ls"];
    #[cfg(windows)]
    let unlisted_controls = [
        r"C:\Windows\System32\where.exe",
        r"C:\Windows\System32\findstr.exe",
    ];
    for control in unlisted_controls {
        if std::path::Path::new(control).exists() && !allowed_shells.iter().any(|s| s == control) {
            assert!(
                !aterm_containment::is_process_allowed(control),
                "unlisted binary {control} must be denied"
            );
            break;
        }
    }
}
