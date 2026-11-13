// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use aterm_core::shell_integration::ShellType;
use std::ffi::{OsStr, OsString};

#[test]
fn the_integration_is_the_resolved_programs() {
    let comspec = |hint: Option<&OsStr>| {
        assert_eq!(hint, None);
        OsString::from(r"C:\WINDOWS\system32\cmd.exe")
    };
    assert_eq!(super::spawn_shell_type(None, comspec), ShellType::Cmd);
    let git_bash = |hint: Option<&OsStr>| {
        assert_eq!(hint, Some(OsStr::new("gitbash")));
        OsString::from(r"C:\Program Files\Git\bin\bash.exe")
    };
    assert_eq!(
        super::spawn_shell_type(Some("gitbash"), git_bash),
        ShellType::Bash
    );
    let pwsh = |_: Option<&OsStr>| OsString::from(r"C:\Program Files\PowerShell\7\pwsh.exe");
    assert_eq!(super::spawn_shell_type(None, pwsh), ShellType::PowerShell);
}

#[test]
fn the_default_tab_is_integrated_as_the_program_it_runs() {
    let (program, _) = aterm_pty::select_shell_with_origin(None);
    assert_eq!(
        super::detect_spawn_shell(None),
        ShellType::detect(&program.to_string_lossy())
    );
}

/// A SESSION'S SHELL RUNS JOBS BY WHAT ATERM LAUNCHED (H2 land): pwsh alone runs its
/// commands in its own group; the shell it integrated is the one launched; an adopted shell is
/// the one its process maps (this test's own process: no shell aterm knows, so it runs jobs,
/// failing closed), never the integration a fresh spawn would have chosen. NEGATIVE CONTROL
/// (scratch): `shell_runs_jobs` true for pwsh fails the first assertion.
#[test]
fn a_sessions_shell_runs_jobs_by_what_aterm_launched() {
    assert!(!super::shell_runs_jobs(ShellType::PowerShell));
    for shell in [
        ShellType::Zsh,
        ShellType::Bash,
        ShellType::Fish,
        ShellType::Unknown,
    ] {
        assert!(super::shell_runs_jobs(shell), "{shell:?}");
    }
    assert_eq!(
        super::launched_shell(false, 0, Some(ShellType::PowerShell), None),
        ShellType::PowerShell
    );
    let me = i32::try_from(std::process::id()).expect("a pid");
    assert_ne!(
        super::launched_shell(true, me, Some(ShellType::PowerShell), None),
        ShellType::PowerShell,
        "an adopted shell is read from its process"
    );
}

/// M2 land (audit 4 review): an adopted shell is the one its image's path names, else, its
/// file gone since its exec (a PowerShell upgrade under a running pwsh, where macOS's
/// `proc_pidpath` names no path), the one a name the kernel keeps for its exec names, so
/// that pwsh still runs every command in its own group after a seamless update and a
/// restore reads its children. A path decides over the kernel's names; none that names a
/// shell is `Unknown`, which runs jobs. NEGATIVE CONTROL (scratch): no fallback to the
/// kernel's names fails the first assertion with `Unknown`.
#[test]
fn an_adopted_shell_whose_file_is_gone_is_read_by_its_kernel_name() {
    let pwsh = || vec!["pwsh".to_owned()];
    assert_eq!(super::adopted_shell(None, pwsh), ShellType::PowerShell);
    assert!(!super::shell_runs_jobs(super::adopted_shell(None, pwsh)));
    assert_eq!(
        [
            super::adopted_shell(Some("/bin/zsh".into()), pwsh),
            super::adopted_shell(None, || vec!["QZP7M2C".to_owned()]),
            super::adopted_shell(None, Vec::new),
        ],
        [ShellType::Zsh, ShellType::Unknown, ShellType::Unknown]
    );
}
