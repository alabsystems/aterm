// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn the_answer_file_is_one_xml_document_with_every_pass() {
    let xml = autounattend("p<w>&'\"");
    assert!(xml.starts_with("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<unattend "));
    for pass in ["windowsPE", "specialize", "oobeSystem"] {
        assert_eq!(
            xml.matches(&format!("<settings pass=\"{pass}\">")).count(),
            1,
            "{pass}"
        );
    }
    assert_eq!(
        xml.matches("<settings ").count(),
        xml.matches("</settings>").count()
    );
    assert_eq!(
        xml.matches("<component ").count(),
        xml.matches("</component>").count()
    );
    assert_eq!(xml.matches("p&lt;w&gt;&amp;&apos;&quot;").count(), 2);
    assert!(!xml.contains("p<w>"));
    assert!(xml.contains("<AcceptEula>true</AcceptEula>"));
    assert!(xml.contains(&format!("<Value>{EDITION}</Value>")));
    assert!(xml.contains("PreventDeviceEncryption"));
    for key in LAB_CONFIG {
        assert!(
            xml.contains(&format!("/v {key} /t REG_DWORD /d 1 /f")),
            "{key}"
        );
    }
    assert_eq!(
        xml.matches("processorArchitecture=\"arm64\"").count(),
        xml.matches("<component ").count()
    );
}

#[test]
fn the_first_logon_finds_its_script_on_any_letter() {
    let c = first_logon_command();
    assert!(c.contains("%d:\\aterm-lane.ps1"));
    assert!(c.contains("in (D E F"));
    assert!(autounattend("x").contains(&aterm_codec::xml::escape(&c)));
    let script = first_logon_script();
    assert!(!script.contains("\r\r"));
    assert!(script.contains("OpenSSH.Server~~~~0.0.1.0"));
    assert!(script.contains("administrators_authorized_keys"));
    assert!(script.contains(FIRST_LOGON_DONE.rsplit('\\').next().unwrap_or("")));
    assert!(script.contains(&format!("$runner = '{RUNNER}'")));
    let keys = RUNNER_KEYS.rsplit('\\').next().unwrap_or("");
    assert!(script.contains(&format!("Join-Path $ssh '{keys}'")));
    assert!(script.contains(&format!("__PROGRAMDATA__/ssh/{keys}")));
}

#[test]
fn single_quotes_double_inside_a_powershell_literal() {
    assert_eq!(ps_quote("it's"), "'it''s'");
    assert_eq!(ps_quote(""), "''");
}

#[test]
fn windows_command_lines_quote_as_command_line_to_argv_reads_them() {
    assert_eq!(cmd_quote("cargo"), "cargo");
    assert_eq!(cmd_quote("-p"), "-p");
    assert_eq!(cmd_quote(""), "\"\"");
    assert_eq!(cmd_quote("a b"), "\"a b\"");
    assert_eq!(cmd_quote("say \"hi\""), "^\"say \\^\"hi\\^\"^\"");
    assert_eq!(cmd_quote("C:\\dir\\"), "C:\\dir\\");
    assert_eq!(cmd_quote("C:\\a dir\\"), "\"C:\\a dir\\\\\"");
    assert_eq!(cmd_quote("a&b"), "\"a&b\"");
    assert_eq!(cmd_quote("a\"&b"), "^\"a\\^\"^&b^\"");
    assert_eq!(cmd_quote("x \"y\" (z)|w"), "^\"x \\^\"y\\^\" ^(z^)^|w^\"");
    assert_eq!(cmd_quote("a\"|\"b"), "^\"a\\^\"^|\\^\"b^\"");
}

/// cmd.exe's reading of a quoted word: quote state toggled at every `"`,
/// `^` outside quotes escaping the next character; what is left is the
/// command line the program gets.
fn as_cmd_hands_on(word: &str) -> Result<String, char> {
    let mut out = String::new();
    let mut quoted = false;
    let mut chars = word.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                quoted = !quoted;
                out.push(c);
            }
            '^' if !quoted => out.extend(chars.next()),
            '&' | '|' | '<' | '>' | '(' | ')' if !quoted => return Err(c),
            c => out.push(c),
        }
    }
    if quoted {
        return Err('"');
    }
    Ok(out)
}

/// CommandLineToArgvW's reading of one word (backslashes before a quote
/// halved, a quote after an odd run kept, quotes otherwise toggling).
fn as_argv_reads(word: &str) -> String {
    let mut out = String::new();
    let mut backslashes = 0usize;
    for c in word.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                out.extend(std::iter::repeat_n('\\', backslashes / 2));
                if backslashes % 2 == 1 {
                    out.push('"');
                }
                backslashes = 0;
            }
            c => {
                out.extend(std::iter::repeat_n('\\', backslashes));
                backslashes = 0;
                out.push(c);
            }
        }
    }
    out.extend(std::iter::repeat_n('\\', backslashes));
    out
}

#[test]
fn a_quoted_word_reaches_the_program_whole_through_cmd() {
    for word in [
        "a\"&b",
        "x \"y\" (z)|w",
        "a\"|\"b",
        "\"a\" & \"b\"",
        "say \"hi\"",
        "C:\\a dir\\",
        "<in> ^caret^",
        "plain",
    ] {
        let q = cmd_quote(word);
        let handed =
            as_cmd_hands_on(&q).unwrap_or_else(|c| panic!("{word:?}: cmd acts on {c:?} in {q}"));
        assert_eq!(as_argv_reads(&handed), word, "{q}");
    }
    assert_eq!(as_cmd_hands_on("\"a\\\"&b\""), Err('&'));
    assert_eq!(as_cmd_hands_on("\"a\\\"^&b\""), Err('"'));
}

#[test]
fn an_encoded_command_is_the_scripts_utf16le_in_base64() {
    // "dir" in UTF-16LE is 64 00 69 00 72 00.
    assert!(powershell_command("dir").ends_with(" -EncodedCommand ZABpAHIA"));
}

/// The host's web2 goes where the `web2` row looks on Windows, checked by its digest, and a
/// plan without it provisions again once it reads.
#[test]
fn the_guest_gets_the_hosts_web2_where_the_web2_row_looks() {
    let sha = "ab".repeat(32);
    let plan = provision_plan(Some(&sha));
    let web2 = plan.last().unwrap_or_else(|| unreachable!());
    assert_eq!(web2.label, "web2");
    assert!(web2.script.contains(&ps_quote(web2_path())) && web2.script.contains(&sha));
    let without = provision_plan(None);
    assert!(without.iter().all(|s| s.label != "web2"));
    assert_ne!(fingerprint(&plan), fingerprint(&without));
}

/// A guest's [`STAMP`] holds this value: a fold that moved it would provision every guest
/// again.
#[test]
fn a_stamped_fingerprint_does_not_move() {
    let steps = [
        Step {
            label: "one",
            script: "Write-Output é".to_owned(),
            runner: false,
            reboot: true,
        },
        Step {
            label: "two",
            script: String::new(),
            runner: true,
            reboot: false,
        },
    ];
    assert_eq!(fingerprint(&steps), "c8b6e042f2c6b27c");
    assert_eq!(fingerprint(&[]), "cbf29ce484222325");
}

#[test]
fn the_plan_pins_what_it_downloads_and_its_fingerprint_follows_it() {
    let steps = provision_steps();
    let all: String = steps.iter().map(|s| s.script.as_str()).collect();
    assert!(all.contains(GIT.2));
    assert!(all.contains(PWSH.2));
    assert!(all.contains(MSYS2_ZSH.1) && all.contains(&ps_quote(DEVELOPER_MODE_KEY)));
    let git = steps
        .iter()
        .position(|s| s.script.contains(GIT.2))
        .unwrap_or_else(|| unreachable!());
    let zsh = steps
        .iter()
        .position(|s| s.script.contains(MSYS2_ZSH.1))
        .unwrap_or_else(|| unreachable!());
    assert!(
        git < zsh && !steps[zsh].runner,
        "zsh unpacks into Git's tree, as its owner"
    );
    assert!(all.contains(RUSTUP.1));
    assert!(all.contains(&format!("toolchain install {RUST}")));
    assert!(all.contains("x86_64-pc-windows-msvc"));
    for c in VS_COMPONENTS {
        assert!(all.contains(c), "{c}");
    }
    let terminal = steps
        .iter()
        .filter(|s| s.runner)
        .map(|s| s.script.as_str())
        .collect::<String>();
    assert!(terminal.contains(&format!("-MainPackage '{TERMINAL_PACKAGE}_8wekyb3d8bbwe'")));
    assert!(
        steps
            .iter()
            .filter(|s| !s.runner)
            .all(|s| !s.script.contains("Add-AppxPackage"))
    );
    let mut changed = steps.clone();
    changed[0].script.push(' ');
    assert_ne!(fingerprint(&steps), fingerprint(&changed));
    let mut moved = steps.clone();
    moved[0].runner = !moved[0].runner;
    assert_ne!(fingerprint(&steps), fingerprint(&moved));
    let mut restarted = steps.clone();
    restarted[0].reboot = !restarted[0].reboot;
    assert_ne!(fingerprint(&steps), fingerprint(&restarted));
    assert_eq!(fingerprint(&steps), fingerprint(&provision_steps()));
    let at = steps
        .iter()
        .position(|s| s == &desktop_step())
        .unwrap_or_else(|| unreachable!());
    let desktop = &steps[at];
    assert!(desktop.reboot && !desktop.runner);
    assert_eq!(steps.iter().filter(|s| s.reboot).count(), 1);
    assert_eq!(
        steps.get(at + 1..at + 4),
        Some(&[japanese_installed_step(), wsl_distro_step(), symlink_step()][..])
    );
    let developer = steps
        .iter()
        .position(|s| s.script.contains(&ps_quote(DEVELOPER_MODE_KEY)))
        .unwrap_or_else(|| unreachable!());
    assert!(developer < at && !steps[developer].runner);
    assert!(symlink_step().runner);
    assert!(desktop.script.contains("AutoAdminLogon -Value '1'"));
    assert!(
        desktop
            .script
            .contains(&format!("DefaultUserName -Value '{RUNNER}'"))
    );
    assert!(all.contains(&ps_quote(TASKS)));
    assert!(all.contains(WSL.2) && all.contains(WSL_DISTRO.2));
    let feature = steps
        .iter()
        .position(|s| s.script.contains("Microsoft-Windows-Subsystem-Linux"))
        .unwrap_or_else(|| unreachable!());
    assert!(feature < at && !steps[feature].runner);
    let distro = wsl_distro_step();
    assert!(distro.runner);
    assert!(distro.script.contains(&format!(
        "$dir = Join-Path $env:LOCALAPPDATA '{WSL_DIR}\\{}'",
        WSL_DISTRO.0
    )));
    assert!(
        distro
            .script
            .contains(&format!("--import '{}' $dir $f --version 1", WSL_DISTRO.0))
    );
}

/// Every provisioning step parses as PowerShell, read by the pinned pwsh's parser (Windows
/// PowerShell 5.1's grammar for these scripts), so a step whose script does not parse fails
/// here rather than in the guest, where it fails provisioning and reverts the VM. Negative
/// control: the link step's first throw, `"… exited $LASTEXITCODE: $made"`, which provisioning
/// met on 2026-10-29 (`$name:` reads as a scope or drive qualifier), does not parse.
#[test]
fn every_provisioning_step_parses_as_powershell() {
    let pwsh = aterm_uds::host::need_tool("pwsh");
    let dir = aterm_tempfile::tempdir().expect("scratch");
    let steps = provision_steps();
    for (i, s) in steps.iter().enumerate() {
        std::fs::write(dir.path().join(format!("{i:02}.ps1")), &s.script).expect("a step");
    }
    let unparsed = symlink_step().script.replace(
        "exited $($LASTEXITCODE): $made",
        "exited $LASTEXITCODE: $made",
    );
    assert_ne!(
        unparsed,
        symlink_step().script,
        "the control replaced the line"
    );
    std::fs::write(dir.path().join("control.ps1"), unparsed).expect("the control");
    let check = format!(
        "Get-ChildItem -Path {} -Filter *.ps1 | Sort-Object Name | ForEach-Object {{ \
         $errors = $null; \
         [void][System.Management.Automation.Language.Parser]::ParseFile($_.FullName, [ref]$null, [ref]$errors); \
         $_.BaseName + ' ' + @($errors).Count }}",
        ps_quote(&dir.path().to_string_lossy())
    );
    // A hang detector: the pinned pwsh's first exec on a Mac may wait on its assessment.
    let out = aterm_uds::deadline::output_within(
        aterm_uds::spawnfd::command(&pwsh).args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            &check,
        ]),
        aterm_uds::deadline::FIRST_EXEC_HANG,
    )
    .expect("pwsh runs");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout);
    let mut errors: Vec<(String, usize)> = text
        .lines()
        .filter_map(|l| l.trim().split_once(' '))
        .map(|(name, n)| (name.to_owned(), n.parse().expect("a count")))
        .collect();
    let control = errors.pop().expect("the control's line");
    assert_eq!(control.0, "control");
    assert!(control.1 > 0, "the control parsed: {text}");
    let want: Vec<(String, usize)> = (0..steps.len()).map(|i| (format!("{i:02}"), 0)).collect();
    assert_eq!(
        errors, want,
        "a step that does not parse (by its index in the plan)"
    );
}

/// The Build Tools step's argument list as Windows PowerShell builds it, read by the pinned
/// pwsh (the same unrolling rules) with `Test-Path` answering whether the Build Tools are
/// installed: `modify` comes first, a word of its own, only where they are (a plan that adds
/// a component to them), and every component follows `--add`. Negative control: the step's
/// line before this fix, `if (…) { @('modify') } else { @() }`, makes the installed list one
/// string, `modify--quiet --wait …`, the bootstrapper's verb lost.
#[test]
fn the_build_tools_take_modify_as_a_word_of_its_own_where_they_are_installed() {
    let pwsh = aterm_uds::host::need_tool("pwsh");
    let built = |installed: bool, arguments: &str| {
        let script = format!(
            "function Test-Path {{ ${installed} }}\n{arguments}\
             $arguments.GetType().Name + ' ' + ($arguments -join '|')\n"
        );
        let out = aterm_uds::deadline::output_within(
            aterm_uds::spawnfd::command(&pwsh).args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                &script,
            ]),
            aterm_uds::deadline::FIRST_EXEC_HANG,
        )
        .expect("pwsh runs");
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    };
    let words: Vec<String> = [
        "--quiet",
        "--wait",
        "--norestart",
        "--nocache",
        "--installPath",
        VS_PATH,
    ]
    .into_iter()
    .map(str::to_owned)
    .chain(
        VS_COMPONENTS
            .iter()
            .flat_map(|c| ["--add".to_owned(), (*c).to_owned()]),
    )
    .collect();
    assert_eq!(
        built(true, &vs_arguments()),
        format!("Object[] modify|{}", words.join("|"))
    );
    assert_eq!(
        built(false, &vs_arguments()),
        format!("Object[] {}", words.join("|"))
    );
    let step = provision_steps()
        .into_iter()
        .find(|s| s.script.contains("vs_BuildTools.exe"))
        .unwrap_or_else(|| unreachable!());
    assert!(step.script.contains(&vs_arguments()));
    assert!(
        step.script
            .contains("Start-Process -FilePath $f -ArgumentList $arguments -Wait -PassThru")
    );
    let before = vs_arguments().replacen(
        "$verb = @(if (Test-Path (Join-Path 'C:\\BuildTools' 'Common7\\Tools\\VsDevCmd.bat')) { 'modify' })",
        "$verb = if (Test-Path (Join-Path 'C:\\BuildTools' 'Common7\\Tools\\VsDevCmd.bat')) { @('modify') } else { @() }",
        1,
    );
    assert_ne!(before, vs_arguments(), "the control replaced the line");
    assert_eq!(
        built(true, &before),
        format!("String modify{}", words.join(" "))
    );
}

#[test]
fn a_desktop_run_is_a_task_of_the_runners_in_its_session() {
    let line = exec_command(
        SRC,
        &cargo_env(Arch::Aarch64),
        &["cargo".into(), "test".into()],
    );
    let bat = desktop_batch(&line, "aterm-lane-1-2");
    assert!(bat.starts_with("@echo off\r\nchcp 65001 >nul\r\n"));
    assert!(bat.contains(&format!("{line}\r\nset LANE_CODE=%ERRORLEVEL%\r\n")));
    assert!(bat.contains(&format!(
        ">\"{TASKS}\\aterm-lane-1-2.code\" echo %LANE_CODE%\r\n"
    )));
    assert!(bat.ends_with("exit /b %LANE_CODE%\r\n"));
    for how in [Desktop::Console, Desktop::Hidden, Desktop::Started] {
        let ps = desktop_runner("aterm-lane-1-2", how);
        assert!(ps.contains(&format!("$bat = '{TASKS}\\aterm-lane-1-2.cmd'")));
        assert!(ps.contains(&format!("$log = '{TASKS}\\aterm-lane-1-2.log'")));
        assert!(ps.contains(&format!(
            "New-ScheduledTaskPrincipal -UserId '{RUNNER}' -LogonType Interactive -RunLevel Limited"
        )));
        assert!(ps.contains(&format!("if {} {{ break }}", desktop_condition())));
        assert!(ps.contains(&format!("AddSeconds({DESKTOP_WITHIN_SECS})")));
        assert!(ps.contains(&format!("-gt {TASK_START_WITHIN_SECS}")));
        assert!(ps.trim_end().ends_with("exit $code"));
        assert_eq!(
            ps.contains("-Argument \"/d /c $bat > $log 2>&1\""),
            how == Desktop::Console
        );
        assert_eq!(
            ps.contains(
                "'System32\\conhost.exe') -Argument \"--headless cmd.exe /d /c $bat > $log 2>&1\""
            ),
            how != Desktop::Console
        );
        let finally = ps.find("} finally {").unwrap_or(usize::MAX);
        let unregister = ps.find("Unregister-ScheduledTask");
        if how == Desktop::Started {
            assert!(unregister.is_none() && finally == usize::MAX);
        } else {
            assert!(unregister.is_some_and(|u| u > finally));
        }
    }
    assert!(desktop_check().contains(DESKTOP_UP));
}

#[test]
fn the_desk_steps_install_the_ime_the_languages_and_the_display_driver() {
    let steps = provision_steps();
    let at = |s: &Step| steps.iter().position(|x| x == s).unwrap_or(usize::MAX);
    let desktop = at(&desktop_step());
    for s in [japanese_step(), languages_step(), display_step()] {
        assert!(at(&s) < desktop, "{}", s.label);
    }
    let ja = japanese_step();
    assert!(!ja.runner && ja.script.contains(&ps_quote(JA_CAPABILITY)));
    assert!(
        ja.script
            .contains(&format!("AddSeconds({CAPABILITY_WITHIN_SECS})"))
    );
    let installed = japanese_installed_step();
    assert!(!installed.runner && installed.script.contains(&ps_quote(JA_CAPABILITY)));
    let pending = "-notin 'Installed', 'InstallPending'";
    assert_eq!(ja.script.matches(pending).count(), 2);
    assert!(installed.script.contains("-ne 'Installed'"));
    assert!(!installed.script.contains("InstallPending"));
    assert!(languages_step().runner);
    let display = display_step();
    assert!(!display.runner);
    for (file, sha) in VIOGPUDO_FILES {
        assert!(
            display.script.contains(&format!("@('{file}', '{sha}')")),
            "{file}"
        );
    }
    assert!(display.script.contains(&ps_quote(VIOGPUDO_GUEST)));
}

#[test]
fn an_exec_sets_the_environment_and_returns_the_commands_status() {
    let s = exec_command(
        SRC,
        &cargo_env(Arch::X86_64),
        &["cargo".into(), "build".into(), "-p".into(), "aterm".into()],
    );
    assert!(s.starts_with("set \"RUSTUP_TOOLCHAIN="));
    assert!(s.contains("set \"CARGO_BUILD_TARGET=x86_64-pc-windows-msvc\" && "));
    assert!(s.contains(" && cd /d C:\\lane\\src && "));
    assert!(s.ends_with(" && cargo build -p aterm 2>&1"));
    assert!(
        !cargo_env(Arch::Aarch64)
            .iter()
            .any(|(k, _)| k == "CARGO_BUILD_TARGET")
    );
}
