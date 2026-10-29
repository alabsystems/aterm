// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

// A command's marks fall around the command alone: its D before any user prompt hook can run a
// program, its C after every user preexec hook. aterm hears a program after the command's D (or
// before its C) as no command's, so a hook's program never names a command (H2 rule 3).

/// Where each of `needles` first stands in `seg`, in order, or a panic naming the one missing.
#[cfg(unix)]
fn places(label: &str, seg: &str, needles: &[&str]) -> Vec<usize> {
    needles
        .iter()
        .map(|needle| {
            seg.find(needle)
                .unwrap_or_else(|| panic!("{label}: {needle:?} missing from {seg:?}"))
        })
        .collect()
}

/// Stand-ins for the programs aterm's own prompt code runs, placed ahead of the shell's on its
/// `PATH`: `rm` (a re-key file's, a body pointer's) prints `RM-RAN` and removes; `stty -a` reports
/// a terminal left `-isig`, so the tty repair after a failed command runs `stty isig …`, which
/// prints `STTY-RAN`. Each marker is spelled so a shell's echo of the typed line holds neither.
#[cfg(unix)]
fn aterms_own_programs(dir: &Path) {
    write_executable(
        &dir.join("rm"),
        "#!/bin/sh\nprintf 'RM-%s\\n' RAN\nexec /bin/rm \"$@\"\n",
    );
    write_executable(
        &dir.join("stty"),
        "#!/bin/sh\ncase \"$1\" in -a) echo 'speed 9600 baud; -isig icanon' ;; \
         *) printf 'STTY-%s\\n' RAN ;; esac\n",
    );
}

/// That none of aterm's own programs ([`aterms_own_programs`]) ran before `d` in `seg`, and that
/// each of `after` ran after it.
#[cfg(unix)]
fn none_before_d(label: &str, seg: &str, d: usize, after: &[&str]) {
    for marker in ["RM-RAN", "STTY-RAN"] {
        assert!(
            !seg[..d].contains(marker),
            "{label}: aterm's {marker} before the command's D: {seg:?}"
        );
    }
    for marker in after {
        assert!(
            seg[d..].contains(marker),
            "{label}: {marker} after the command's D: {seg:?}"
        );
    }
}

/// Send `line`, wait for `last` after `from`, and return the output between with where it ends.
#[cfg(unix)]
fn ran_line(sh: &mut LiveShell, line: &str, last: &str, from: usize) -> (String, usize) {
    sh.send(line);
    let end = sh.wait_for_after(last, from);
    (stdout_between(sh, from, end), end)
}

#[cfg(unix)]
#[test]
fn a_zsh_writes_a_commands_d_before_its_prompt_hooks_and_its_c_after_its_preexec_hooks() {
    let zsh = need_zsh();
    let fx = LiveFixture::new();
    std::fs::create_dir(fx.home.join("QZPd1")).expect("an AUTO_CD directory");
    // A user's `precmd` function (zsh runs it before every `precmd_functions` entry), a precmd
    // hook put ahead of aterm's, and a preexec hook after aterm's: each runs a program.
    let rc = "setopt autocd\n\
              precmd() { /bin/echo \"PRECMD-FN-RAN-$?\" }\n\
              zqahead() { /bin/echo AHEAD-HOOK-RAN }\n\
              precmd_functions=(zqahead $precmd_functions)\n\
              zqpre() { /bin/echo PREEXEC-HOOK-RAN }\n\
              preexec_functions+=(zqpre)\n";
    let mut sh = spawn_live_zsh(zsh, &fx, rc, &[]);
    sh.send("cd ~");
    let mut at = sh.wait_for_after("AHEAD-HOOK-RAN", 0);
    at = sh.wait_for_after("AHEAD-HOOK-RAN", at);
    for (line, code) in [("false", 1), ("./QZPd1", 0), ("./nosuch.sh", 127)] {
        let (seg, end) = ran_line(&mut sh, line, "AHEAD-HOOK-RAN", at);
        let [pre, c, d, func, ahead] = places(
            line,
            &seg,
            &[
                "PREEXEC-HOOK-RAN",
                "\u{1b}]133;C\u{7}",
                &format!("\u{1b}]133;D;{code}\u{7}"),
                &format!("PRECMD-FN-RAN-{code}"),
                "AHEAD-HOOK-RAN",
            ],
        )[..] else {
            unreachable!("five places")
        };
        assert!(
            pre < c && c < d && d < func && d < ahead,
            "{line}: the preexec hook before C, D before the precmd function and the hook \
             ahead of aterm's: {seg:?}"
        );
        at = end;
    }
    let (seg, _) = ran_line(&mut sh, "print -r -- \"AT=${PWD:t}\"", "AHEAD-HOOK-RAN", at);
    assert!(seg.contains("AT=QZPd1"), "AUTO_CD changed into it: {seg:?}");
    sh.finish();
}

/// bash writes a command's D before the user's PROMPT_COMMAND, and before every program aterm's
/// own prompt code runs (H2 land): the re-key file's and the body pointer's `rm` and the tty
/// repair's `stty` come after it, so none is heard as the command's. A hook put ahead of aterm's
/// trampoline after the integration loaded (zoxide's `PROMPT_COMMAND="hook;$PROMPT_COMMAND"`, run
/// again by a re-sourced ~/.bashrc, and on bash 5.1+ an array element before it) runs after D from
/// the next prompt on (`__aterm_pc_first`). NEGATIVE CONTROLS (scratch): the integration with D
/// written after `__aterm_rekey_check`, the tty repair and the body check (as before) fails on
/// `RM-RAN` and `STTY-RAN` before D; without `__aterm_pc_first`, `AHEAD-RAN` comes before D.
#[cfg(unix)]
#[test]
fn a_bash_writes_a_commands_d_before_its_prompt_command() {
    let mut shells = ShellLanes::default();
    for bash in shells.bashes() {
        let fx = LiveFixture::new();
        std::fs::create_dir(fx.home.join("QZPd1")).expect("an AUTO_CD directory");
        aterms_own_programs(&fx.foreign);
        let rekey = fx.home.join("qzrekey");
        let pointer = fx.home.join("qzpointer");
        let rc = "shopt -s autocd 2>/dev/null\n\
                  PROMPT_COMMAND='/bin/echo \"PC-RAN-$?\"'\n\
                  qzahead() { local s=$?; /usr/bin/printf 'AHEAD-%s\\n' RAN; return $s; }\n\
                  qzaheadb() { local s=$?; /usr/bin/printf 'AHEADB-%s\\n' RAN; return $s; }\n";
        let extra = [
            (REKEY_PATH_VAR, rekey.to_str().expect("UTF-8")),
            (BODY_POINTER_VAR, pointer.to_str().expect("UTF-8")),
        ];
        let mut sh = spawn_live_bash(bash, &fx, rc, &extra);
        sh.send("cd ~");
        let mut at = sh.wait_for_after("PC-RAN-0", 0);
        at = sh.wait_for_after("PC-RAN-0", at);
        let autocd = shell_has_autocd(bash);
        let both = format!(
            "printf x >{}; printf y >{}; false",
            rekey.display(),
            pointer.display()
        );
        let mut rows: Vec<(&str, u8, &[&str])> = vec![
            ("false", 1, &["STTY-RAN"]),
            ("./nosuch.sh", 127, &["STTY-RAN"]),
            (both.as_str(), 1, &["RM-RAN", "STTY-RAN"]),
        ];
        if autocd {
            rows.push(("./QZPd1", 0, &[]));
        }
        for (line, code, after) in rows {
            let ran = format!("PC-RAN-{code}");
            let (seg, end) = ran_line(&mut sh, line, &ran, at);
            let label = format!("{bash}: {line}");
            let [c, d, hook] = places(
                &label,
                &seg,
                &["\u{1b}]133;C", &format!("\u{1b}]133;D;{code}\u{7}"), &ran],
            )[..] else {
                unreachable!("three places")
            };
            assert!(
                c < d && d < hook,
                "{label}: D before the user's PROMPT_COMMAND: {seg:?}"
            );
            none_before_d(&label, &seg, d, after);
            at = end;
        }
        assert!(
            !rekey.exists() && !pointer.exists(),
            "{bash}: the re-key file and the body pointer were consumed"
        );
        // The line that puts a hook ahead runs it before its own D once (SPECS); every line
        // after it has its D first.
        let mut aheads = vec![(
            "PROMPT_COMMAND=\"qzahead;${PROMPT_COMMAND#;}\"",
            "AHEAD-RAN",
        )];
        if shell_has_array_prompt_command(bash) {
            aheads.push((
                "PROMPT_COMMAND=(qzaheadb \"${PROMPT_COMMAND[@]}\")",
                "AHEADB-RAN",
            ));
        }
        for (define, ahead) in aheads {
            (_, at) = ran_line(&mut sh, define, "PC-RAN-0", at);
            for (line, code) in [("false", 1), ("./nosuch.sh", 127)] {
                sh.send(line);
                let end = sh
                    .wait_for_after(&format!("PC-RAN-{code}"), at)
                    .max(sh.wait_for_after(ahead, at));
                let seg = stdout_between(&sh, at, end);
                let label = format!("{bash}: {define}, then {line}");
                let [d, hook] =
                    places(&label, &seg, &[&format!("\u{1b}]133;D;{code}\u{7}"), ahead])[..]
                else {
                    unreachable!("two places")
                };
                assert!(d < hook, "{label}: D before the hook put ahead: {seg:?}");
                at = end;
            }
        }
        sh.finish();
    }
}

/// Whether `bash` runs each element of an array `PROMPT_COMMAND` (bash 5.1 and later).
#[cfg(unix)]
fn shell_has_array_prompt_command(bash: &str) -> bool {
    shell_command(bash)
        .args([
            "--noprofile",
            "--norc",
            "-c",
            "(( BASH_VERSINFO[0] > 5 || (BASH_VERSINFO[0] == 5 && BASH_VERSINFO[1] >= 1) ))",
        ])
        .output()
        .is_ok_and(|out| out.status.success())
}

/// Whether `bash` has `shopt autocd` (bash 4 and later; macOS's /bin/bash 3.2 has none).
#[cfg(unix)]
fn shell_has_autocd(bash: &str) -> bool {
    shell_command(bash)
        .args(["--noprofile", "--norc", "-c", "shopt -s autocd"])
        .output()
        .is_ok_and(|out| out.status.success())
}

/// fish writes a command's C after a user's preexec handler and its D before a user's postexec
/// handler and before the re-key file's `rm` (H2 land: the re-key is taken with builtins first,
/// its file removed after D). NEGATIVE CONTROL (scratch): `__aterm_rekey_check` before D (as
/// before) fails on `RM-RAN` before D.
#[cfg(unix)]
#[test]
fn a_fish_writes_a_commands_c_after_a_preexec_handler_defined_after_aterms() {
    let fish = need_fish();
    let fx = LiveFixture::new();
    aterms_own_programs(&fx.foreign);
    let rekey = fx.home.join("qzrekey");
    let mut sh = spawn_live_fish(
        fish,
        &fx,
        &[(REKEY_PATH_VAR, rekey.to_str().expect("UTF-8"))],
    );
    // fish echoes the typed line on its pty: the markers are spelled so the echo holds neither.
    sh.send("function zqpre --on-event fish_preexec; /bin/echo PREEXEC-HOOK-(echo RAN); end");
    sh.send("function zqpost --on-event fish_postexec; /bin/echo POST-HOOK-(echo RAN); end");
    let mut at = sh.wait_for_after("POST-HOOK-RAN", 0);
    let rekeyed = format!("printf x >{}; false", rekey.display());
    let rows: [(&str, u8, &[&str]); 3] = [
        ("false", 1, &[]),
        ("./nosuch.sh", 127, &[]),
        (rekeyed.as_str(), 1, &["RM-RAN"]),
    ];
    for (line, code, after) in rows {
        let (seg, end) = ran_line(&mut sh, line, "POST-HOOK-RAN", at);
        let [pre, c, d, post] = places(
            line,
            &seg,
            &[
                "PREEXEC-HOOK-RAN",
                // aterm's marks end in BEL; fish 4's own (`133;C;cmdline_url=`) in ST.
                "\u{1b}]133;C\u{7}",
                &format!("\u{1b}]133;D;{code}\u{7}"),
                "POST-HOOK-RAN",
            ],
        )[..] else {
            unreachable!("four places")
        };
        assert!(
            pre < c && c < d && d < post,
            "{line}: the preexec handler before C, D before the postexec handler: {seg:?}"
        );
        none_before_d(line, &seg, d, after);
        at = end;
    }
    assert!(!rekey.exists(), "the re-key file was consumed");
    sh.finish();
}

#[cfg(unix)]
#[test]
fn a_pwsh_writes_a_commands_d_before_the_users_prompt_runs() {
    let pwsh = need_pwsh();
    let script = format!(
        "{}/src/scripts/aterm_shell_integration.ps1",
        env!("CARGO_MANIFEST_DIR")
    );
    let mut cmd = shell_command(pwsh);
    cmd.args([
        "-NoProfile",
        "-NonInteractive",
        "-Command",
        "function prompt { [Console]::Write('USER-PROMPT-RAN'); 'PS> ' }; \
         . $env:ATERM_TEST_SCRIPT; $null = prompt; [Console]::Write('FIRST-DONE'); \
         $now = Get-Date; \
         [pscustomobject]@{ CommandLine = 'Get-Item nosuch'; ExecutionStatus = 'Completed'; \
           StartExecutionTime = $now; EndExecutionTime = $now } | Add-History; \
         [Console]::Write((prompt))",
    ])
    .env("ATERM_TEST_SCRIPT", &script)
    .env("POWERSHELL_TELEMETRY_OPTOUT", "1")
    .env("POWERSHELL_UPDATECHECK", "Off");
    let out = cmd
        .output()
        .unwrap_or_else(|error| panic!("spawn {pwsh}: {error}"));
    let stdout = String::from_utf8_lossy(&out.stdout);
    let (_, second) = stdout.split_once("FIRST-DONE").unwrap_or_else(|| {
        panic!(
            "the first prompt ran: {stdout:?}; stderr: {:?}",
            String::from_utf8_lossy(&out.stderr)
        )
    });
    let [d, user] = places("pwsh", second, &["\u{1b}]133;D;0", "USER-PROMPT-RAN"])[..] else {
        unreachable!("two places")
    };
    assert!(d < user, "D before the user's prompt runs: {second:?}");
}
