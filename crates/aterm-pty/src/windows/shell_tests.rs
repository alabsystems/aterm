// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// Ask `CreateProcessW` what it makes of `program` as `lpApplicationName` —
/// the exact parameter [`super::super::spawn_shell_with_pid`] passes the
/// resolved shell in.
fn create_process_verdict(program: &str) -> Result<(), u32> {
    let app_w = wide_nul(OsStr::new(program));
    // SAFETY: zeroed STARTUPINFOW is valid once `cb` is set; PROCESS_INFORMATION
    // is POD out-memory; `app_w` is a NUL-terminated wide string alive across
    // the call and every other pointer is NULL.
    unsafe {
        let mut si: ffi::STARTUPINFOW = std::mem::zeroed();
        si.cb = u32::try_from(std::mem::size_of::<ffi::STARTUPINFOW>()).unwrap();
        let mut pi: ffi::PROCESS_INFORMATION = std::mem::zeroed();
        let ok = ffi::CreateProcessW(
            app_w.as_ptr(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            0,
            ffi::CREATE_SUSPENDED,
            std::ptr::null_mut(),
            std::ptr::null(),
            &si,
            &mut pi,
        );
        if ok == 0 {
            return Err(std::io::Error::last_os_error().raw_os_error().unwrap_or(0) as u32);
        }
        ffi::TerminateProcess(pi.hProcess, 1);
        ffi::CloseHandle(pi.hThread);
        ffi::CloseHandle(pi.hProcess);
        Ok(())
    }
}

/// Verify what `CreateProcessW` accepts and rejects as `lpApplicationName`.
#[test]
fn create_process_application_name_contract() {
    /// `ERROR_FILE_NOT_FOUND`.
    const NOT_FOUND: u32 = 2;
    /// `ERROR_INVALID_NAME`.
    const INVALID_NAME: u32 = 123;

    let sysroot = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".to_string());
    let cmd = format!("{sysroot}\\System32\\cmd.exe");
    assert!(
        Path::new(&cmd).is_file(),
        "premise: cmd.exe must exist at {cmd}"
    );

    assert_eq!(
        create_process_verdict(&cmd),
        Ok(()),
        "an absolute backslash path runs"
    );
    assert_eq!(
        create_process_verdict(&cmd.replace('\\', "/")),
        Ok(()),
        "forward slashes are a legal Windows spelling — Win32 canonicalizes them"
    );
    assert_eq!(
        create_process_verdict(cmd.trim_end_matches(".exe")),
        Err(NOT_FOUND),
        "lpApplicationName gets NO default extension appended"
    );
    assert_eq!(
        create_process_verdict("cmd.exe"),
        Err(NOT_FOUND),
        "lpApplicationName is NEVER PATH-searched (this test's cwd is the crate dir)"
    );
    assert_eq!(
        create_process_verdict(&format!("\"{cmd}\"")),
        Err(INVALID_NAME),
        "`shell` names one program: a quoted value is a filename containing a quote"
    );
    assert_eq!(
        create_process_verdict(&format!("\"{cmd}\" /K dir")),
        Err(INVALID_NAME),
        "arguments belong in `shell_args`, never inside `shell`"
    );
    assert_eq!(
        create_process_verdict("%COMSPEC%"),
        Err(NOT_FOUND),
        "`%VAR%` is never expanded by CreateProcessW"
    );

    let script_dir = std::env::temp_dir().join(format!("aterm-lpan-{}", std::process::id()));
    std::fs::create_dir_all(&script_dir).unwrap();
    let script = script_dir.join("probe.cmd");
    std::fs::write(&script, b"@echo off\r\nexit /b 0\r\n").unwrap();
    let batch_verdict = create_process_verdict(&script.display().to_string());
    let _ = std::fs::remove_dir_all(&script_dir);
    assert_eq!(
        batch_verdict,
        Ok(()),
        "a full path to a batch file IS a runnable lpApplicationName \
         (CreateProcess re-invokes the command interpreter for it)"
    );
}

#[test]
fn path_like_classifier() {
    assert!(is_path_like("C:\\Windows\\cmd.exe"));
    assert!(is_path_like("bin/tool.exe"));
    assert!(is_path_like("C:cmd"));
    assert!(!is_path_like("cmd"));
    assert!(!is_path_like("pwsh"));
}

#[test]
fn search_path_finds_cmd_and_misses_nonsense() {
    let hit = search_path("cmd").expect("cmd.exe must resolve on Windows");
    assert!(
        hit.to_string_lossy()
            .to_ascii_lowercase()
            .ends_with("cmd.exe"),
        "resolved {hit:?}"
    );
    assert!(search_path("aterm-no-such-prog-xyz").is_none());
}

#[test]
fn resolve_program_passes_paths_verbatim_and_falls_back_verbatim() {
    assert_eq!(
        resolve_program_windows("C:\\nonexistent\\x.exe"),
        OsString::from("C:\\nonexistent\\x.exe"),
        "path-like: verbatim, even when absent (CreateProcessW fails cleanly)"
    );
    assert_eq!(
        resolve_program_windows("aterm-no-such-prog-xyz"),
        OsString::from("aterm-no-such-prog-xyz"),
        "unresolved bare name: verbatim fallback"
    );
    let cmd = resolve_program_windows("cmd");
    assert!(
        cmd.to_string_lossy()
            .to_ascii_lowercase()
            .ends_with("cmd.exe"),
        "bare name resolves via SearchPathW: {cmd:?}"
    );
}

#[test]
fn parse_pathext_defaults_and_drops_junk() {
    assert_eq!(
        parse_pathext(None),
        [".COM", ".EXE", ".BAT", ".CMD"].map(String::from).to_vec(),
        "unset PATHEXT falls back to the OS default head"
    );
    assert_eq!(
        parse_pathext(Some("  ")),
        [".COM", ".EXE", ".BAT", ".CMD"].map(String::from).to_vec(),
        "blank PATHEXT falls back too"
    );
    assert_eq!(
        parse_pathext(Some(".COM;.EXE;.BAT;.CMD;.PS1")),
        [".COM", ".EXE", ".BAT", ".CMD", ".PS1"]
            .map(String::from)
            .to_vec()
    );
    assert_eq!(
        parse_pathext(Some(".EXE;;junk;.;  .CMD  ")),
        [".EXE", ".CMD"].map(String::from).to_vec(),
        "dotless/empty entries are dropped, whitespace trimmed"
    );
}

#[test]
fn resolve_program_finds_cmd_and_bat_shims_via_pathext() {
    let sysroot = match std::env::var_os("SystemRoot") {
        Some(r) => r,
        None => aterm_spec::host::could_not_run("no %SystemRoot% here"),
    };
    let shim = Path::new(&sysroot).join("System32").join("winrm.cmd");
    if !shim.is_file() {
        aterm_spec::host::could_not_run("this Windows SKU ships no System32\\winrm.cmd");
    }
    let hit = resolve_program_windows("winrm");
    assert!(
        hit.to_string_lossy()
            .to_ascii_lowercase()
            .ends_with("winrm.cmd"),
        "bare .cmd shim must resolve via the PATHEXT retry: {hit:?}"
    );
}

#[test]
fn exec_command_takes_precedence_and_argv_is_verbatim() {
    let cmd = ["cmd", "/c", "echo hi"].map(OsString::from).to_vec();
    let ov = vec!["ignored".to_string()];
    let (program, argv) = resolve_spawn_target(None, None, Some(&ov), Some(&cmd));
    assert!(
        program
            .to_string_lossy()
            .to_ascii_lowercase()
            .ends_with("cmd.exe"),
        "-e program is PATH-resolved: {program:?}"
    );
    assert_eq!(
        argv,
        ["cmd", "/c", "echo hi"].map(OsString::from).to_vec(),
        "argv[0] stays the name as GIVEN (Unix parity)"
    );
}

#[test]
fn shell_override_selects_and_resolves_the_named_shell() {
    let (program, argv) = resolve_spawn_target(Some("cmd"), None, None, None);
    assert!(
        program
            .to_string_lossy()
            .to_ascii_lowercase()
            .ends_with("cmd.exe"),
        "override 'cmd' resolves to cmd.exe: {program:?}"
    );
    assert_eq!(argv, vec![program], "bare interactive argv is [shell]");
    let (p2, _) = resolve_spawn_target(Some("C:\\x\\my.exe"), None, None, None);
    assert_eq!(p2, OsString::from("C:\\x\\my.exe"), "path-like verbatim");
}

#[test]
fn shell_args_shape_the_interactive_argv() {
    let args = vec!["-l".to_string(), "-i".to_string()];
    let (program, argv) = resolve_spawn_target(Some("cmd"), Some(&args), None, None);
    assert_eq!(argv.len(), 3, "argv = [shell, -l, -i]");
    assert_eq!(argv[0], program);
    assert_eq!(argv[1], OsString::from("-l"));
    assert_eq!(argv[2], OsString::from("-i"));
}

#[test]
fn discover_shell_unknown_alias_is_none() {
    assert!(
        discover_shell("definitely-not-a-shell").is_none(),
        "unknown alias falls through to SearchPathW"
    );
}

#[test]
fn wsl_shell_args_survive_the_integration_argv_override() {
    let args = vec!["-d".to_string(), "Debian".to_string()];
    let ov = vec![
        "wsl".to_string(),
        "--exec".to_string(),
        "/bin/sh".to_string(),
        "-c".to_string(),
        "exec bash".to_string(),
    ];
    let (program, argv) = resolve_spawn_target(Some("wsl"), Some(&args), Some(&ov), None);
    assert!(
        program
            .to_string_lossy()
            .to_ascii_lowercase()
            .contains("wsl"),
        "override 'wsl' still resolves to wsl.exe: {program:?}"
    );
    assert_eq!(
        argv,
        [
            "wsl",
            "-d",
            "Debian",
            "--exec",
            "/bin/sh",
            "-c",
            "exec bash"
        ]
        .map(OsString::from)
        .to_vec()
    );
    let (_, plain) = resolve_spawn_target(Some("wsl"), None, Some(&ov), None);
    assert_eq!(plain, ov.iter().map(OsString::from).collect::<Vec<_>>());
}

#[test]
fn non_wsl_argv_override_still_replaces_argv_entirely() {
    let args = vec!["-l".to_string()];
    let ov = vec![
        "bash".to_string(),
        "--rcfile".to_string(),
        "C:\\cache\\bash\\rcfile".to_string(),
    ];
    let (_, argv) = resolve_spawn_target(Some("cmd"), Some(&args), Some(&ov), None);
    assert_eq!(
        argv,
        ["bash", "--rcfile", "C:\\cache\\bash\\rcfile"]
            .map(OsString::from)
            .to_vec(),
        "a non-wsl override must be verbatim, shell_args dropped as before"
    );
}

#[test]
fn is_wsl_shell_matches_the_stem_case_insensitively() {
    assert!(is_wsl_shell(OsStr::new("wsl")));
    assert!(is_wsl_shell(OsStr::new("C:\\Windows\\System32\\WSL.EXE")));
    assert!(!is_wsl_shell(OsStr::new("C:\\x\\wslconfig.exe")));
    assert!(!is_wsl_shell(OsStr::new("bash.exe")));
}

/// Verify that `classify_shell_name` matches the resolution performed by `resolve_shell_name`.
#[test]
fn classify_shell_name_is_the_resolution_the_spawn_performs() {
    let cmd = classify_shell_name(OsStr::new("cmd"));
    match &cmd {
        ShellResolution::Resolved(p) => assert!(
            p.to_string_lossy()
                .to_ascii_lowercase()
                .ends_with("cmd.exe"),
            "bare `cmd` resolves to cmd.exe: {p:?}"
        ),
        other => panic!("a bare name on PATH must be Resolved, got {other:?}"),
    }
    for spelling in ["CMD", "cmd.exe"] {
        assert!(
            matches!(
                classify_shell_name(OsStr::new(spelling)),
                ShellResolution::Resolved(_)
            ),
            "{spelling:?} must resolve"
        );
    }
    assert_eq!(
        classify_shell_name(OsStr::new("aterm-no-such-shell-xyz")),
        ShellResolution::Unresolved
    );
    for path in [
        "C:\\Windows\\System32\\cmd.exe",
        "C:/Windows/System32/cmd.exe",
        "C:\\Program Files\\Git\\bin\\bash.exe",
        "\\\\server\\share\\sh.exe",
        "C:cmd",
        "C:\\nonexistent\\x.exe",
    ] {
        assert_eq!(
            classify_shell_name(OsStr::new(path)),
            ShellResolution::Verbatim(OsString::from(path)),
            "{path:?} is path-like and must pass through verbatim"
        );
    }
    for name in [
        "cmd",
        "aterm-no-such-shell-xyz",
        "C:\\Windows\\System32\\cmd.exe",
    ] {
        let expected = match classify_shell_name(OsStr::new(name)) {
            ShellResolution::Resolved(p) | ShellResolution::Verbatim(p) => p,
            ShellResolution::Unresolved => OsString::from(name),
        };
        assert_eq!(
            resolve_shell_name(OsStr::new(name)),
            expected,
            "resolve_shell_name must not drift from classify_shell_name for {name:?}"
        );
    }
}

/// Verify that `select_shell_with_origin` agrees with `select_shell` and reports the deciding arm.
#[test]
fn select_shell_with_origin_names_the_arm_select_shell_took() {
    let (program, origin) = select_shell_with_origin(Some(OsStr::new("cmd")));
    assert_eq!(origin, ShellOrigin::Override);
    assert_eq!(program, select_shell(Some(OsStr::new("cmd"))));
    assert!(
        program
            .to_string_lossy()
            .to_ascii_lowercase()
            .ends_with("cmd.exe"),
        "an override resolves exactly as select_shell resolves it: {program:?}"
    );
    assert_eq!(
        select_shell_with_origin(Some(OsStr::new("aterm-no-such-shell-xyz"))),
        (
            OsString::from("aterm-no-such-shell-xyz"),
            ShellOrigin::Override
        )
    );
    let (program, origin) = select_shell_with_origin(Some(OsStr::new("")));
    assert_eq!((program.clone(), origin), select_shell_with_origin(None));
    assert_eq!(program, select_shell(None));
    match origin {
        ShellOrigin::Override => panic!("no override was passed"),
        ShellOrigin::Pwsh => assert!(search_path("pwsh").is_some()),
        ShellOrigin::PowerShell => assert!(search_path("powershell").is_some()),
        ShellOrigin::Comspec | ShellOrigin::CmdLiteral => {}
    }
    if search_path("pwsh").is_some() {
        assert_eq!(
            origin,
            ShellOrigin::Pwsh,
            "pwsh on PATH is the first default"
        );
    }
}

#[test]
fn path_like_names_bypass_discovery() {
    assert_eq!(
        resolve_shell_name(OsStr::new("C:\\Program Files\\Git\\bin\\bash.exe")),
        OsString::from("C:\\Program Files\\Git\\bin\\bash.exe")
    );
}
