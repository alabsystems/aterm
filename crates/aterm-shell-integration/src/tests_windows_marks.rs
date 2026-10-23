// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

/// The `133;D` codes the console drew, in order.
#[cfg(windows)]
fn drawn_exit_codes(seen: &[u8]) -> Vec<String> {
    let text = String::from_utf8_lossy(seen);
    text.split("\u{1b}]133;D;")
        .skip(1)
        .map(|rest| {
            rest.split(['\u{7}', ';', '\u{1b}'])
                .next()
                .unwrap_or_default()
                .to_owned()
        })
        .collect()
}

/// One step of [`test_powershell_reports_the_exit_code_of_the_line_that_failed`]: the line
/// typed, the output that shows it is running before a Ctrl+C is sent (`None`: no Ctrl+C),
/// and the code its `133;D` must carry.
#[cfg(windows)]
struct ExitStep {
    line: &'static str,
    interrupt_after: Option<&'static str>,
    code: &'static str,
}

/// STATUS_CONTROL_C_EXIT, as `$LASTEXITCODE` prints it.
#[cfg(windows)]
const CONTROL_C_EXIT: &str = "-1073741510";

/// The lines, in order, and what each must report.
#[cfg(windows)]
const EXIT_STEPS: &[ExitStep] = &[
    ExitStep {
        line: "cmd /c exit 7",
        interrupt_after: None,
        code: "7",
    },
    ExitStep {
        line: "Write-Error boom",
        interrupt_after: None,
        code: "1",
    },
    ExitStep {
        line: "throw 'x'",
        interrupt_after: None,
        code: "1",
    },
    ExitStep {
        line: "Get-Item C:\\nope-aterm-test",
        interrupt_after: None,
        code: "1",
    },
    ExitStep {
        line: "cmd /c exit 7",
        interrupt_after: None,
        code: "7",
    },
    ExitStep {
        line: "Get-Date | Out-Null",
        interrupt_after: None,
        code: "0",
    },
    ExitStep {
        line: "Write-Host ('zz' + '-sleeping'); Start-Sleep 60",
        interrupt_after: Some("zz-sleeping"),
        code: CONTROL_C_EXIT,
    },
    ExitStep {
        line: "Write-Error boom",
        interrupt_after: None,
        code: "1",
    },
    ExitStep {
        line: "ping -n 60 127.0.0.1",
        interrupt_after: Some("127.0.0.1:"),
        code: CONTROL_C_EXIT,
    },
    ExitStep {
        line: "Write-Host ('zz' + '-sleeping-again'); Start-Sleep 60",
        interrupt_after: Some("zz-sleeping-again"),
        code: CONTROL_C_EXIT,
    },
    ExitStep {
        line: "Get-Item C:\\nope-aterm-test",
        interrupt_after: None,
        code: "1",
    },
    ExitStep {
        line: "# just a comment",
        interrupt_after: None,
        code: "0",
    },
    ExitStep {
        line: "cmd /c exit 0",
        interrupt_after: None,
        code: "0",
    },
    ExitStep {
        line: "}",
        interrupt_after: None,
        code: "1",
    },
    ExitStep {
        line: "Get-Date | Out-Null",
        interrupt_after: None,
        code: "0",
    },
    ExitStep {
        line: "Get-Date | Out-Null)",
        interrupt_after: None,
        code: "1",
    },
    ExitStep {
        line: "Get-Item C:\\nope-aterm-test",
        interrupt_after: None,
        code: "1",
    },
    ExitStep {
        line: "}",
        interrupt_after: None,
        code: "1",
    },
    ExitStep {
        line: "# just a comment",
        interrupt_after: None,
        code: "0",
    },
    ExitStep {
        line: "cmd /c \"echo e 1>&2 & exit 7\" 2>&1",
        interrupt_after: None,
        code: "7",
    },
    ExitStep {
        line: "cmd /c \"echo e 1>&2 & exit 7\" 2>&1",
        interrupt_after: None,
        code: "7",
    },
    ExitStep {
        line: "Write-Error boom",
        interrupt_after: None,
        code: "1",
    },
    ExitStep {
        line: "$PSNativeCommandUseErrorActionPreference = $true",
        interrupt_after: None,
        code: "0",
    },
    ExitStep {
        line: "cmd /c exit 9",
        interrupt_after: None,
        code: "9",
    },
    ExitStep {
        line: "cmd /c exit 9",
        interrupt_after: None,
        code: "9",
    },
    ExitStep {
        line: "$PSNativeCommandUseErrorActionPreference = $false",
        interrupt_after: None,
        code: "0",
    },
];

/// The code a failed line reports, at a live prompt in an aterm tab, on Windows PowerShell
/// 5.1 and on pwsh 7: see [`EXIT_STEPS`] and the script's "The exit code of a line that
/// failed".
#[cfg(windows)]
#[test]
fn test_powershell_reports_the_exit_code_of_the_line_that_failed() {
    on_each_powershell("exit codes", |exe| {
        let world = PowerShellWorld::new();
        let dir = &world.root;
        let quoted = |path: &Path| path.display().to_string().replace('\'', "''");
        let profile = dir.path().join("profile.ps1");
        let script = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src")
            .join("scripts")
            .join("aterm_shell_integration.ps1");
        let body = [
            format!(
                "Set-PSReadLineOption -HistorySavePath '{}' -HistorySaveStyle SaveNothing",
                quoted(&dir.path().join("history.txt"))
            ),
            "$global:__exit_prompts = 0".to_owned(),
            "function global:prompt { $global:__exit_prompts += 1; 'X' + $global:__exit_prompts + '> ' }"
                .to_owned(),
            format!(". '{}'", quoted(&script)),
        ]
        .join("\r\n");
        std::fs::write(&profile, body).expect("write the profile");
        let command = format!("{}. '{}'", PowerShellWorld::home_guard(), quoted(&profile));
        let argv: Vec<std::ffi::OsString> = [
            exe.as_os_str(),
            "-NoProfile".as_ref(),
            "-NoLogo".as_ref(),
            "-NoExit".as_ref(),
            "-ExecutionPolicy".as_ref(),
            "Bypass".as_ref(),
            "-Command".as_ref(),
            command.as_ref(),
        ]
        .into_iter()
        .map(std::ffi::OsStr::to_os_string)
        .collect();
        let env_add: Vec<(String, String)> = [
            ("ATERM_CHILD", "1"),
            ("ATERM_SHELL_INTEGRATION_INSTALLED", ""),
            ("ATERM_SHELL_NONCE", ""),
            ("PSModulePath", ""),
        ]
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect();

        let mut shell = ConptyShell::spawn(&world, &argv, &env_add);
        for (n, step) in EXIT_STEPS.iter().enumerate() {
            shell.wait_for(
                &format!("X{}>", n + 1),
                &format!("{exe:?} before {:?}", step.line),
            );
            let typed = shell.seen.len();
            shell.type_keys(&format!("{}\r", step.line));
            if let Some(running) = step.interrupt_after {
                let deadline = std::time::Instant::now() + ConptyShell::HANG;
                while !String::from_utf8_lossy(&shell.seen[typed..]).contains(running) {
                    match shell
                        .rx
                        .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
                    {
                        Ok(chunk) => shell.seen.extend_from_slice(&chunk),
                        Err(error) => panic!(
                            "{exe:?} {:?}: {error:?} before it drew {running:?}; it drew: {}",
                            step.line,
                            shell.drawn()
                        ),
                    }
                }
                shell.type_keys("\x03");
            }
        }
        shell.wait_for(
            &format!("X{}>", EXIT_STEPS.len() + 1),
            &format!("{exe:?} after the last line"),
        );
        let codes = drawn_exit_codes(&shell.seen);
        let drawn = shell.drawn();
        shell.type_keys("exit\r");
        shell.wait_for_exit(&format!("{exe:?} exit"));
        drop(shell);
        let expected: Vec<&str> = EXIT_STEPS.iter().map(|step| step.code).collect();
        assert_eq!(
            codes,
            expected,
            "{exe:?}: the 133;D code of each line ({:?}); the console drew: {drawn}",
            EXIT_STEPS.iter().map(|step| step.line).collect::<Vec<_>>()
        );
    });
}

/// Git Bash for the live test: `$ATERM_TEST_GIT_BASH`, else Git for Windows' own `bash.exe`
/// where it is installed.
#[cfg(windows)]
fn git_bash_exe() -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os("ATERM_TEST_GIT_BASH") {
        let explicit = PathBuf::from(explicit);
        assert!(
            explicit.is_file(),
            "ATERM_TEST_GIT_BASH points at {explicit:?}, which is not a file"
        );
        return Some(explicit);
    }
    ["ProgramW6432", "ProgramFiles"]
        .into_iter()
        .filter_map(std::env::var_os)
        .map(|root| PathBuf::from(root).join("Git").join("bin").join("bash.exe"))
        .find(|candidate| candidate.is_file())
}

/// The `633;E` command lines the console drew, in order, as encoded.
#[cfg(windows)]
fn drawn_command_lines(seen: &[u8]) -> Vec<String> {
    let text = String::from_utf8_lossy(seen);
    text.split("\u{1b}]633;E;")
        .skip(1)
        .map(|rest| {
            rest.split(['\u{7}', ';', '\u{1b}'])
                .next()
                .unwrap_or_default()
                .to_owned()
        })
        .collect()
}

/// Git Bash records the line as typed and gives a line that opens with a subshell its
/// block.
#[cfg(windows)]
const GIT_BASH_LINES: &[(&str, &str, &str)] = &[
    ("ls /nope-aterm-test", r"ls\x20/nope-aterm-test", "2"),
    ("(exit 3)", r"(exit\x203)", "3"),
    ("true && (exit 4)", r"true\x20&&\x20(exit\x204)", "4"),
    ("( true )", r"(\x20true\x20)", "0"),
    (" echo spaced", r"echo\x20spaced", "0"),
    ("echo done-$((6*7))", r"echo\x20done-$((6*7))", "0"),
];

/// A Git Bash tab's blocks at a live prompt in a ConPTY: on bash 5.3+, which Git for
/// Windows ships, every line bash saved is recorded as typed and starts its block from
/// PS0, whatever its first command ([`GIT_BASH_LINES`]).
#[cfg(windows)]
#[test]
fn test_git_bash_records_the_typed_line_and_marks_a_subshell_line() {
    let Some(bash) = git_bash_exe() else {
        aterm_spec::host::could_not_run(
            "git bash blocks: no Git for Windows bash.exe and no ATERM_TEST_GIT_BASH",
        );
    };
    let world = PowerShellWorld::new();
    let dir = world.root.path();
    let slashed = |path: &Path| path.display().to_string().replace('\\', "/");
    let script = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join("scripts")
        .join("aterm_shell_integration.bash");
    let rc = dir.join("rc.bash");
    std::fs::write(
        &rc,
        format!(
            "HISTFILE={}\nHISTCONTROL=ignorespace\nalias ls='ls -F'\nPS1='G\\#> '\n. {}\n",
            posix_quoted(&dir.join("history")),
            posix_quoted(&script)
        ),
    )
    .expect("write the rc");
    let argv: Vec<std::ffi::OsString> = vec![
        bash.into_os_string(),
        "--noprofile".into(),
        "--rcfile".into(),
        slashed(&rc).into(),
        "-i".into(),
    ];
    let env_add: Vec<(String, String)> = [
        ("ATERM_CHILD", "1"),
        ("ATERM_SHELL_INTEGRATION_INSTALLED", ""),
        ("ATERM_SHELL_NONCE", ""),
        ("ATERM_DISABLE_PROMPT_TITLES", ""),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_owned(), value.to_owned()))
    .collect();
    let mut shell = ConptyShell::spawn(&world, &argv, &env_add);
    shell.wait_for("G1>", "git bash first prompt");
    let version_line = "echo \"bash-$((40+2)) ${BASH_VERSINFO[0]}.${BASH_VERSINFO[1]}\"";
    shell.type_keys(&format!("{version_line}\r"));
    shell.wait_for("G2>", "git bash version");
    let drawn = String::from_utf8_lossy(&shell.seen).into_owned();
    let version = drawn
        .split("bash-42 ")
        .nth(1)
        .and_then(|rest| rest.split(['\r', '\n']).next())
        .unwrap_or_default()
        .to_owned();
    let skip = shell.seen.len();
    for (n, &(line, _, _)) in GIT_BASH_LINES.iter().enumerate() {
        shell.type_keys(&format!("{line}\r"));
        shell.wait_for(&format!("G{}>", n + 3), &format!("git bash after {line:?}"));
    }
    let tail = &shell.seen[skip..];
    let lines = drawn_command_lines(tail);
    let codes = drawn_exit_codes(tail);
    let drawn = shell.drawn();
    shell.type_keys("exit\r");
    shell.wait_for_exit("git bash exit");
    drop(shell);
    let (major, minor) = version.split_once('.').unwrap_or_default();
    let funsub = major.parse::<u32>().is_ok_and(|major| {
        major > 5 || (major == 5 && minor.parse::<u32>().is_ok_and(|minor| minor >= 3))
    });
    if !funsub {
        aterm_spec::host::could_not_run(format!(
            "git bash blocks: bash {version:?} is older than 5.3, which PS0 needs"
        ));
    }
    assert_eq!(
        (lines, codes),
        (
            GIT_BASH_LINES
                .iter()
                .map(|&(_, typed, _)| typed.to_owned())
                .collect::<Vec<_>>(),
            GIT_BASH_LINES
                .iter()
                .map(|&(_, _, code)| code.to_owned())
                .collect::<Vec<_>>()
        ),
        "bash {version}: each line's 633;E and 133;D; the console drew: {drawn}"
    );
}

/// A Git Bash whose `/tmp` went away while it ran still runs every line typed, records it
/// as typed and gives a line that opens with a subshell its block ([`GIT_BASH_LINES`]),
/// fresh or with the PS0 a loader of v0.107.0-v0.108.0 left in a running shell.
#[cfg(windows)]
#[test]
fn test_git_bash_whose_tmp_went_away_runs_every_line_as_typed() {
    let Some(bash) = git_bash_exe() else {
        aterm_spec::host::could_not_run(
            "git bash without /tmp: no Git for Windows bash.exe and no ATERM_TEST_GIT_BASH",
        );
    };
    let script = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join("scripts")
        .join("aterm_shell_integration.bash");
    let slashed = |path: &Path| path.display().to_string().replace('\\', "/");
    for (start, ps0) in [
        ("fresh", ""),
        ("v0.108.0's PS0", "PS0='${ __aterm_ps0; }'\n"),
    ] {
        let world = PowerShellWorld::new();
        let dir = world.root.path();
        let tmp = dir.join("tmp");
        let rc = dir.join("rc.bash");
        std::fs::write(
            &rc,
            format!(
                "unset TMPDIR\nHISTFILE={}\nHISTCONTROL=ignorespace\nalias ls='ls -F'\n\
                 PS1='T\\#> '\n{ps0}. {}\n",
                posix_quoted(&dir.join("history's")),
                posix_quoted(&script)
            ),
        )
        .expect("write the rc");
        let argv: Vec<std::ffi::OsString> = vec![
            bash.clone().into_os_string(),
            "--noprofile".into(),
            "--rcfile".into(),
            slashed(&rc).into(),
            "-i".into(),
        ];
        let mut shell = ConptyShell::spawn(&world, &argv, &git_bash_test_env(false));
        shell.wait_for("T1>", &format!("git bash first prompt ({start})"));
        shell.type_keys(
            "echo \"R$((40+2))=${BASH_VERSINFO[0]}.${BASH_VERSINFO[1]} $(cygpath -m /tmp)\
             =E$((40+2))\"\r",
        );
        shell.wait_for("T2>", &format!("git bash probe ({start})"));
        let report = drawn_report(&shell.seen);
        let (version, folder) = report.split_once(' ').unwrap_or_default();
        let funsub = version.split_once('.').is_some_and(|(major, minor)| {
            major.parse::<u32>().is_ok_and(|major| {
                major > 5 || (major == 5 && minor.parse::<u32>().is_ok_and(|minor| minor >= 3))
            })
        });
        if !funsub {
            aterm_spec::host::could_not_run(format!(
                "git bash without /tmp: bash {version:?} is older than 5.3, which PS0 needs"
            ));
        }
        assert!(
            !folder.is_empty(),
            "git bash ({start}): the probe drew no /tmp; the console drew: {}",
            shell.drawn()
        );
        if !folder.eq_ignore_ascii_case(&slashed(&tmp)) {
            aterm_spec::host::could_not_run(format!(
                "git bash without /tmp: /tmp is {folder:?}, the TEMP of an MSYS2 process this \
                 test did not start (a Git Bash the suite runs from?), not this test's {tmp:?}; \
                 run the suite from cmd or pwsh with no Git Bash open"
            ));
        }
        std::fs::remove_dir_all(&tmp).expect("take the shell's /tmp away");
        let skip = shell.seen.len();
        for (n, &(line, _, _)) in GIT_BASH_LINES.iter().enumerate() {
            shell.type_keys(&format!("{line}\r"));
            shell.wait_for(
                &format!("T{}>", n + 3),
                &format!("git bash without /tmp ({start}) after {line:?}"),
            );
        }
        let tail = &shell.seen[skip..];
        let lines = drawn_command_lines(tail);
        let codes = drawn_exit_codes(tail);
        let drawn = shell.drawn();
        shell.type_keys("exit\r");
        shell.wait_for_exit("git bash exit");
        drop(shell);
        assert_eq!(
            (lines, codes),
            (
                GIT_BASH_LINES
                    .iter()
                    .map(|&(_, typed, _)| typed.to_owned())
                    .collect::<Vec<_>>(),
                GIT_BASH_LINES
                    .iter()
                    .map(|&(_, _, code)| code.to_owned())
                    .collect::<Vec<_>>()
            ),
            "git bash without /tmp ({start}): each line's 633;E and 133;D; the console \
             drew: {drawn}"
        );
    }
}

/// The win32-input-mode record aterm writes for Ctrl+J (`aterm-types`'
/// `win32_ctrl_character_record`): the J key (`VK_J` 74, scan 0x24), char LF,
/// `LEFT_CTRL_PRESSED`, down then up.
#[cfg(windows)]
const CTRL_J_RECORD: &str = "\x1b[74;36;10;1;8;1_\x1b[74;36;10;0;8;1_";

/// Ctrl+J, as aterm writes it, accepts the line in an aterm tab: the readline layer's
/// AcceptLine in PSReadLine (5.1's 2.0.0 and pwsh 7's), and readline's accept-line in Git
/// Bash, which reads the record's LF.
#[cfg(windows)]
#[test]
fn test_ctrl_j_accepts_the_line_in_powershell_and_git_bash() {
    on_each_powershell("Ctrl+J", |exe| {
        let world = PowerShellWorld::new();
        let dir = &world.root;
        let quoted = |path: &Path| path.display().to_string().replace('\'', "''");
        let result = dir.path().join("result.txt");
        let profile = dir.path().join("profile.ps1");
        let script = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src")
            .join("scripts")
            .join("aterm_shell_integration.ps1");
        let body = [
            format!(
                "Set-PSReadLineOption -HistorySavePath '{}' -HistorySaveStyle SaveNothing",
                quoted(&dir.path().join("history.txt"))
            ),
            "$global:__j_prompts = 0".to_owned(),
            "function global:prompt { $global:__j_prompts += 1; 'J' + $global:__j_prompts + '> ' }"
                .to_owned(),
            format!(
                "function global:__j_dump {{ \
                 $l = @(); \
                 foreach ($h in [Microsoft.PowerShell.PSConsoleReadLine]::GetHistoryItems()) {{ \
                 $l += 'line=' + $h.CommandLine }}; \
                 [System.IO.File]::WriteAllLines('{}', [string[]]$l) }}",
                quoted(&result)
            ),
            format!(". '{}'", quoted(&script)),
        ]
        .join("\r\n");
        std::fs::write(&profile, body).expect("write the profile");
        let command = format!("{}. '{}'", PowerShellWorld::home_guard(), quoted(&profile));
        let argv: Vec<std::ffi::OsString> = [
            exe.as_os_str(),
            "-NoProfile".as_ref(),
            "-NoLogo".as_ref(),
            "-NoExit".as_ref(),
            "-ExecutionPolicy".as_ref(),
            "Bypass".as_ref(),
            "-Command".as_ref(),
            command.as_ref(),
        ]
        .into_iter()
        .map(std::ffi::OsStr::to_os_string)
        .collect();
        let env_add: Vec<(String, String)> = [
            ("ATERM_CHILD", "1"),
            ("ATERM_SHELL_INTEGRATION_INSTALLED", ""),
            ("ATERM_DISABLE_READLINE_KEYS", ""),
            ("PSModulePath", ""),
        ]
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect();
        let mut shell = ConptyShell::spawn(&world, &argv, &env_add);
        shell.wait_for("J1>", &format!("{exe:?} first prompt"));
        shell.type_keys(&format!("#J1 alpha{CTRL_J_RECORD}"));
        shell.wait_for("J2>", &format!("{exe:?} after Ctrl+J"));
        shell.type_keys("#J2 tail\r");
        shell.wait_for("J3>", &format!("{exe:?} after Enter"));
        shell.type_keys("__j_dump; exit\r");
        shell.wait_for_exit(&format!("{exe:?} dump"));
        let drawn = shell.drawn();
        drop(shell);
        let dump = std::fs::read_to_string(&result)
            .unwrap_or_else(|error| panic!("{exe:?} wrote no dump ({error}); it drew: {drawn}"));
        let history: Vec<&str> = dump
            .lines()
            .filter_map(|line| line.strip_prefix("line="))
            .collect();
        let history = history
            .strip_prefix(&[command.as_str()][..])
            .unwrap_or(&history);
        assert_eq!(
            history,
            ["#J1 alpha", "#J2 tail", "__j_dump; exit"],
            "{exe:?}: the lines Ctrl+J and Enter submitted; the console drew: {drawn}"
        );
    });

    let Some(bash) = git_bash_exe() else {
        aterm_spec::host::could_not_run(
            "Ctrl+J on git bash: no Git for Windows bash.exe and no ATERM_TEST_GIT_BASH",
        );
    };
    let world = PowerShellWorld::new();
    let rc = world.root.path().join("rc.bash");
    std::fs::write(&rc, "HISTFILE=/dev/null\nPS1='B\\#> '\n").expect("write the rc");
    let argv: Vec<std::ffi::OsString> = vec![
        bash.into_os_string(),
        "--noprofile".into(),
        "--rcfile".into(),
        rc.display().to_string().replace('\\', "/").into(),
        "-i".into(),
    ];
    let mut shell = ConptyShell::spawn(&world, &argv, &[]);
    shell.wait_for("B1>", "git bash first prompt");
    shell.type_keys(&format!("echo J-$((40+2)){CTRL_J_RECORD}"));
    shell.wait_for("J-42", "git bash runs the line Ctrl+J ended");
    shell.wait_for("B2>", "git bash prompt after Ctrl+J");
    shell.type_keys("exit\r");
    shell.wait_for_exit("git bash exit");
}

/// The environment every live Git Bash below runs with: an aterm child, the integration
/// loaded fresh and unsigned, no spare, no prompt style of aterm's; `titles` says whether
/// the integration titles the tab.
#[cfg(windows)]
fn git_bash_test_env(titles: bool) -> Vec<(String, String)> {
    [
        ("ATERM_CHILD", "1"),
        ("ATERM_SHELL_INTEGRATION_INSTALLED", ""),
        ("ATERM_SHELL_NONCE", ""),
        ("ATERM_REKEY_PATH", ""),
        ("ATERM_PROMPT_STYLE", ""),
        ("ATERM_DISABLE_PROMPT_TITLES", if titles { "" } else { "1" }),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_owned(), value.to_owned()))
    .collect()
}

/// What a probe line printed between `R42=` and `=E42` (both computed, so the line as
/// typed never matches), the last time it did.
#[cfg(windows)]
fn drawn_report(seen: &[u8]) -> String {
    let text = String::from_utf8_lossy(seen);
    text.rsplit_once("R42=")
        .and_then(|(_, rest)| rest.split_once("=E42"))
        .map(|(report, _)| report.to_owned())
        .unwrap_or_default()
}

/// The `OSC 0` titles the console drew, in order.
#[cfg(windows)]
fn drawn_titles(seen: &[u8]) -> Vec<String> {
    let text = String::from_utf8_lossy(seen);
    text.split("\u{1b}]0;")
        .skip(1)
        .map(|rest| {
            rest.split(['\u{7}', '\u{1b}'])
                .next()
                .unwrap_or_default()
                .to_owned()
        })
        .collect()
}

/// A Git Bash tab starts as the LOGIN shell every Windows terminal starts it as
/// (`BASH_LOGIN_START`): Git's `profile.d/git-prompt.sh` (the user's own
/// `~/.config/git/git-prompt.sh` here, which it sources in its place) loads once, the
/// login profile once and the `~/.bashrc` it reads once, `login_shell` is on, POSIX mode
/// is off again, the history file is bash's own `~/.bash_history`, `ENV` is gone, and
/// the integration is loaded.
#[cfg(windows)]
#[test]
fn test_a_git_bash_tab_starts_as_a_login_git_bash_that_loads_git_prompt_once() {
    let Some(bash) = git_bash_exe() else {
        aterm_spec::host::could_not_run(
            "git bash start: no Git for Windows bash.exe and no ATERM_TEST_GIT_BASH",
        );
    };
    let probe = "echo \"R$((40+2))=loads:$__gp_loads,profile:$__profile_runs,\
                 bashrc:$__bashrc_runs,login:$(shopt -q login_shell && echo y || echo n),\
                 posix:$(shopt -qo posix && echo y || echo n),hist:${HISTFILE##*/},\
                 env:${ENV-unset},integrated:$(type -t __aterm_mark_prompt_start)=E$((40+2))\"";
    let report = |tab: bool| {
        let world = PowerShellWorld::new();
        let home = world.home();
        std::fs::create_dir_all(home.join(".config").join("git")).expect("~/.config/git");
        std::fs::write(
            home.join(".config").join("git").join("git-prompt.sh"),
            "__gp_loads=$((${__gp_loads:-0} + 1))\nPS1='[gp]> '\n",
        )
        .expect("git-prompt.sh");
        std::fs::write(
            home.join(".bash_profile"),
            "__profile_runs=$((${__profile_runs:-0} + 1))\ntest -f ~/.bashrc && . ~/.bashrc\n",
        )
        .expect(".bash_profile");
        std::fs::write(
            home.join(".bashrc"),
            "__bashrc_runs=$((${__bashrc_runs:-0} + 1))\n",
        )
        .expect(".bashrc");
        let base = world.root.path().join("si");
        let injection = prepare_into(ShellType::Bash, &base)
            .expect("prepare")
            .expect("bash is integrated");
        let (mut argv, mut env_add) = if tab {
            (
                injection.argv_override.expect("bash's argv"),
                injection.env_add,
            )
        } else {
            (
                vec![
                    "bash".to_owned(),
                    "--rcfile".to_owned(),
                    base.join("bash").join("rcfile").display().to_string(),
                ],
                vec![(
                    "ATERM_SHELL_INTEGRATION_DIR".to_owned(),
                    base.display().to_string(),
                )],
            )
        };
        argv[0] = bash.display().to_string();
        env_add.extend(git_bash_test_env(false));
        let argv: Vec<std::ffi::OsString> = argv.into_iter().map(Into::into).collect();
        let mut shell = ConptyShell::spawn(&world, &argv, &env_add);
        shell.wait_for(
            "[gp]> ",
            &format!("git bash first prompt (tab start: {tab})"),
        );
        shell.type_keys(&format!("{probe}\r"));
        shell.wait_for("=E42", &format!("git bash probe (tab start: {tab})"));
        let report = drawn_report(&shell.seen);
        shell.type_keys("exit\r");
        shell.wait_for_exit("git bash exit");
        report
    };
    assert_eq!(
        report(true),
        "loads:1,profile:1,bashrc:1,login:y,posix:n,hist:.bash_history,env:unset,\
         integrated:function",
        "a Git Bash tab's start"
    );
    let control = report(false);
    let (loads, rest) = control.split_once(',').unwrap_or_default();
    assert!(
        loads
            .strip_prefix("loads:")
            .and_then(|n| n.parse::<u32>().ok())
            .is_some_and(|n| n > 1),
        "CONTROL: the --rcfile start loads Git's prompt more than once: {control:?}"
    );
    assert_eq!(
        rest,
        "profile:1,bashrc:2,login:n,posix:n,hist:.bash_history,env:unset,\
         integrated:function",
        "CONTROL: the --rcfile start"
    );
}

/// The flags a raw mode clears, as `isig=on echo=off …`, read by a child `stty -a` (the
/// tty as the next command sees it), then `FLAGS42`.
#[cfg(windows)]
const GIT_BASH_FLAGS_LINE: &str = "__s=\" $(command stty -a | tr '\\n' ' ') \"; for __f in isig icanon echo icrnl opost; do case \"$__s\" in *\" -$__f \"*) printf '%s=off ' \"$__f\";; *) printf '%s=on ' \"$__f\";; esac; done; echo FLAGS$((40+2))";

/// After a program that left the tty raw and ran a second or more, whatever its status,
/// the next command's tty is sane again and what is typed is echoed: under MSYS2 a vim
/// killed from outside reports 0, and every line typed after it was run unseen until
/// `stty sane` (the 2026-10-18 verifier's
/// v4-gitbash-killed-app-echo-repair-gated-on-status).
#[cfg(windows)]
#[test]
fn test_git_bash_repairs_its_tty_after_a_raw_program_that_ran_a_second() {
    let Some(bash) = git_bash_exe() else {
        aterm_spec::host::could_not_run(
            "git bash tty repair: no Git for Windows bash.exe and no ATERM_TEST_GIT_BASH",
        );
    };
    let world = PowerShellWorld::new();
    let dir = world.root.path();
    let slashed = |path: &Path| path.display().to_string().replace('\\', "/");
    let script = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join("scripts")
        .join("aterm_shell_integration.bash");
    let rc = dir.join("rc.bash");
    std::fs::write(
        &rc,
        format!(
            "HISTFILE=/dev/null\nPS1='T\\#> '\n. {}\n",
            posix_quoted(&script)
        ),
    )
    .expect("write the rc");
    let argv: Vec<std::ffi::OsString> = vec![
        bash.into_os_string(),
        "--noprofile".into(),
        "--rcfile".into(),
        slashed(&rc).into(),
        "-i".into(),
    ];
    let mut shell = ConptyShell::spawn(&world, &argv, &git_bash_test_env(false));
    let mut prompt = 1;
    let mut run = |shell: &mut ConptyShell, line: &str| {
        shell.wait_for(&format!("T{prompt}>"), &format!("git bash before {line:?}"));
        prompt += 1;
        let from = shell.seen.len();
        shell.type_keys(&format!("{line}\r"));
        shell.wait_for(&format!("T{prompt}>"), &format!("git bash after {line:?}"));
        String::from_utf8_lossy(&shell.seen[from..]).into_owned()
    };
    let flags = |drawn: &str| {
        drawn
            .rsplit_once("FLAGS42")
            .and_then(|(before, _)| {
                before
                    .rfind("isig=")
                    .map(|at| before[at..].trim().to_owned())
            })
            .unwrap_or_default()
    };
    run(
        &mut shell,
        "stty raw -echo; __aterm_line_t0=${EPOCHREALTIME//[!0-9]/}",
    );
    let quick = flags(&run(&mut shell, GIT_BASH_FLAGS_LINE));
    run(&mut shell, "false");
    let repaired_by_failure = flags(&run(&mut shell, GIT_BASH_FLAGS_LINE));
    run(&mut shell, "stty raw -echo; sleep 1.2");
    let after_a_second = flags(&run(&mut shell, GIT_BASH_FLAGS_LINE));
    let typed = run(&mut shell, "echo zz-$((40+2))");
    let drawn = shell.drawn();
    shell.type_keys("exit\r");
    shell.wait_for_exit("git bash exit");
    drop(shell);
    let sane = "isig=on icanon=on echo=on icrnl=on opost=on";
    assert_eq!(
        (
            quick.as_str(),
            repaired_by_failure.as_str(),
            after_a_second.as_str()
        ),
        (
            "isig=off icanon=off echo=off icrnl=off opost=off",
            sane,
            sane
        ),
        "the tty after a quick `stty raw` (CONTROL), after `false`, and after a raw \
         program that ran 1.2 s; the console drew: {drawn}"
    );
    assert!(
        typed.contains("echo zz-$((40+2))") && typed.contains("zz-42"),
        "the line typed after it is echoed and runs: {typed:?}"
    );
}

/// A command line's tab title is the line whole up to 256 characters, and a longer one
/// its first 255 and `…`, in PowerShell (5.1 and 7) and in Git Bash: the 64-character
/// cut before had no mark, and the tab took its cut-off end for the file (the 2026-10-18
/// judges' J4F-1-cut64-middle-cut-misleads).
#[cfg(windows)]
#[test]
fn test_a_command_title_is_the_whole_line_or_marks_its_cut() {
    let short = |program: &str| format!("{program} # {}", "s".repeat(100));
    let long = |program: &str| format!("{program} # {}", "l".repeat(300));
    let cut = |line: &str| format!("{}…", line.chars().take(255).collect::<String>());
    let titled = |seen: &[u8], line: &str| {
        drawn_titles(seen)
            .into_iter()
            .filter(|title| title.contains("zz"))
            .any(|title| title == line)
    };
    on_each_powershell("command titles", |exe| {
        let world = PowerShellWorld::new();
        let dir = &world.root;
        let quoted = |path: &Path| path.display().to_string().replace('\'', "''");
        let profile = dir.path().join("profile.ps1");
        let script = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src")
            .join("scripts")
            .join("aterm_shell_integration.ps1");
        let body = [
            format!(
                "Set-PSReadLineOption -HistorySavePath '{}' -HistorySaveStyle SaveNothing",
                quoted(&dir.path().join("history.txt"))
            ),
            "$global:__title_prompts = 0".to_owned(),
            "function global:prompt { $global:__title_prompts += 1; 'W' + $global:__title_prompts + '> ' }"
                .to_owned(),
            format!(". '{}'", quoted(&script)),
        ]
        .join("\r\n");
        std::fs::write(&profile, body).expect("write the profile");
        let command = format!("{}. '{}'", PowerShellWorld::home_guard(), quoted(&profile));
        let argv: Vec<std::ffi::OsString> = [
            exe.as_os_str(),
            "-NoProfile".as_ref(),
            "-NoLogo".as_ref(),
            "-NoExit".as_ref(),
            "-ExecutionPolicy".as_ref(),
            "Bypass".as_ref(),
            "-Command".as_ref(),
            command.as_ref(),
        ]
        .into_iter()
        .map(std::ffi::OsStr::to_os_string)
        .collect();
        let env_add: Vec<(String, String)> = [
            ("ATERM_CHILD", "1"),
            ("ATERM_SHELL_INTEGRATION_INSTALLED", ""),
            ("ATERM_SHELL_NONCE", ""),
            ("ATERM_DISABLE_PROMPT_TITLES", ""),
            ("PSModulePath", ""),
        ]
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect();
        let mut shell = ConptyShell::spawn(&world, &argv, &env_add);
        let (short, long) = (
            short("Write-Host ('zz' + '-ran')"),
            long("Write-Host ('zz' + '-ran')"),
        );
        shell.wait_for("W1>", &format!("{exe:?} first prompt"));
        shell.type_keys(&format!("{short}\r"));
        shell.wait_for("W2>", &format!("{exe:?} after the short line"));
        shell.type_keys(&format!("{long}\r"));
        shell.wait_for("W3>", &format!("{exe:?} after the long line"));
        let seen = shell.seen.clone();
        let drawn = shell.drawn();
        shell.type_keys("exit\r");
        shell.wait_for_exit(&format!("{exe:?} exit"));
        drop(shell);
        assert!(
            titled(&seen, &short) && titled(&seen, &cut(&long)),
            "{exe:?}: the titles {:?}; the console drew: {drawn}",
            drawn_titles(&seen)
        );
    });

    let Some(bash) = git_bash_exe() else {
        aterm_spec::host::could_not_run(
            "command titles on git bash: no Git for Windows bash.exe and no \
             ATERM_TEST_GIT_BASH",
        );
    };
    let world = PowerShellWorld::new();
    let dir = world.root.path();
    let slashed = |path: &Path| path.display().to_string().replace('\\', "/");
    let script = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join("scripts")
        .join("aterm_shell_integration.bash");
    let rc = dir.join("rc.bash");
    std::fs::write(
        &rc,
        format!(
            "HISTFILE=/dev/null\nPS1='B\\#> '\n. {}\n",
            posix_quoted(&script)
        ),
    )
    .expect("write the rc");
    let argv: Vec<std::ffi::OsString> = vec![
        bash.into_os_string(),
        "--noprofile".into(),
        "--rcfile".into(),
        slashed(&rc).into(),
        "-i".into(),
    ];
    let mut shell = ConptyShell::spawn(&world, &argv, &git_bash_test_env(true));
    let (short, long) = (short("echo zz-ran"), long("echo zz-ran"));
    shell.wait_for("B1>", "git bash first prompt");
    shell.type_keys(&format!("{short}\r"));
    shell.wait_for("B2>", "git bash after the short line");
    shell.type_keys(&format!("{long}\r"));
    shell.wait_for("B3>", "git bash after the long line");
    let seen = shell.seen.clone();
    let drawn = shell.drawn();
    shell.type_keys("exit\r");
    shell.wait_for_exit("git bash exit");
    drop(shell);
    assert!(
        titled(&seen, &short) && titled(&seen, &cut(&long)),
        "git bash: the titles {:?}; the console drew: {drawn}",
        drawn_titles(&seen)
    );
}
