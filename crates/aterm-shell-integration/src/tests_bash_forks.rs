// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

// No subshell at a bash prompt (unit BASH32H): the hooks read a command's output through the
// session's file (`__aterm_out`), never a `$(…)`, so a Ctrl-C that strands bash 3.2's stdio lock
// costs no prompt.

/// The forks a process makes, as kqueue's `NOTE_FORK` tells them: armed when made, read with no
/// wait. Events on one process coalesce until read, so a read that finds one says "at least one";
/// a read that finds none is exact.
#[cfg(target_os = "macos")]
struct ForkWatch {
    kq: std::os::fd::OwnedFd,
}

#[cfg(target_os = "macos")]
impl ForkWatch {
    fn new(pid: u32) -> Self {
        use std::os::fd::{AsRawFd as _, FromRawFd as _};
        // SAFETY: `kqueue` takes no argument; a negative return is its error.
        let kq = unsafe { libc::kqueue() };
        assert!(kq >= 0, "kqueue: {}", std::io::Error::last_os_error());
        // SAFETY: `kq` is the descriptor just made, owned here alone.
        let kq = unsafe { std::os::fd::OwnedFd::from_raw_fd(kq) };
        let change = libc::kevent {
            ident: usize::try_from(pid).expect("pid fits usize"),
            filter: libc::EVFILT_PROC,
            flags: libc::EV_ADD | libc::EV_CLEAR,
            fflags: libc::NOTE_FORK,
            data: 0,
            udata: std::ptr::null_mut(),
        };
        // SAFETY: one change read from `change`, no event list, no timeout (a change list alone
        // returns at once).
        let rc = unsafe {
            libc::kevent(
                kq.as_raw_fd(),
                &raw const change,
                1,
                std::ptr::null_mut(),
                0,
                std::ptr::null(),
            )
        };
        assert_eq!(
            rc,
            0,
            "kevent NOTE_FORK on {pid}: {}",
            std::io::Error::last_os_error()
        );
        Self { kq }
    }

    /// Whether the process forked since the last read.
    fn forked(&self) -> bool {
        use std::os::fd::AsRawFd as _;
        let none = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        let mut events = [libc::kevent {
            ident: 0,
            filter: 0,
            flags: 0,
            fflags: 0,
            data: 0,
            udata: std::ptr::null_mut(),
        }; 4];
        // SAFETY: no change list; `events` holds the 4 entries the call may write, and the zero
        // timeout makes it a poll.
        let n = unsafe {
            libc::kevent(
                self.kq.as_raw_fd(),
                std::ptr::null(),
                0,
                events.as_mut_ptr(),
                4,
                &raw const none,
            )
        };
        let n = usize::try_from(n)
            .unwrap_or_else(|_| panic!("kevent: {}", std::io::Error::last_os_error()));
        events[..n]
            .iter()
            .any(|event| event.fflags & libc::NOTE_FORK != 0)
    }
}

/// The text of `__aterm_out_ok` that finds the session's file, which a control replaces with a
/// shell that has none, whose hooks read by `$(…)` as before.
#[cfg(target_os = "macos")]
const OUT_FILE_FOUND: &str = "__aterm_out_ok() {\n    [[ -n $__aterm_outfile ]] || return 1\n";

/// A PLAIN BASH PROMPT FORKS NOTHING (unit BASH32H): an empty line, a builtin, an assignment and a
/// function's definition (a line before 5.3 only the next prompt hears, from history and bash's
/// command number) each bring their next prompt with no fork of the shell's, under no style and
/// the minimal one (its prompt character set by the hooks), and the session's output file is
/// left empty. Before 4.1, where `BASH_XTRACEFD` is a plain variable, a person's
/// `BASH_XTRACEFD=5` (dotfiles written for a newer bash) costs no fork either. After `false` the
/// tty repair runs `stty`, exec'd: the one program a plain prompt runs, which the watch sees (at
/// least one fork). A `stty` on PATH logs each run, for the repair also runs after a line that
/// spans a second's turn (3.2 times a line by `SECONDS`), so a step whose prompt ran it is
/// excused its fork (exec'd, it takes no lock). NEGATIVE CONTROL, in the test: a shell with no
/// such file reads by `$(…)`, and bash 3.2 forks at its prompts while 5.3, which reads history in
/// process, still does not; scratch: with the version check before `BASH_XTRACEFD` cut, the 3.2
/// row with it set forks at each prompt.
#[cfg(target_os = "macos")]
#[test]
fn an_integrated_bash_forks_no_subshell_at_a_plain_prompt() {
    const CONTROL_REV: &str = "00000000000000f0";
    let plain = ["", ":", "x=1", "f() { :; }"];
    let mut shells = ShellLanes::default();
    for bash in shells.bashes() {
        let level = bash_level(bash);
        let fx = LiveFixture::new();
        let sttybin = fx.home.join("sttybin");
        std::fs::create_dir_all(&sttybin).expect("sttybin");
        let stty_log = fx.home.join("stty.log");
        write_executable(
            &sttybin.join("stty"),
            &format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >>'{}'\nexec /bin/stty \"$@\"\n",
                stty_log.display()
            ),
        );
        let stty_runs = || std::fs::metadata(&stty_log).map_or(0, |meta| meta.len());
        // In the rc: a login shell's profile (macOS's `path_helper`) puts the system's
        // folders ahead of the PATH it was handed.
        std::fs::write(
            fx.home.join(".bashrc"),
            format!(
                "HISTFILE=/dev/null\nHISTCONTROL=ignorespace\ncd /\nPATH='{}':$PATH\n",
                sttybin.display()
            ),
        )
        .expect(".bashrc");
        let ours = install_script_set(&fx.base).expect("install");
        let control = folder_with_edits(
            &fx.base,
            CONTROL_REV,
            &[(
                "bash",
                scripts::BASH,
                &[(OUT_FILE_FOUND, "__aterm_out_ok() {\n    return 1\n")],
            )],
        );
        let rekey = fx.rekey_path();
        let out_file = format!("{rekey}{OUTPUT_SUFFIX}");
        let mut cases = vec![
            (&ours, "none", true, false),
            (&ours, "minimal", true, false),
            (&control, "none", false, false),
        ];
        if level < 401 {
            cases.push((&ours, "none", true, true));
        }
        for (dir, style, fixed, xtracefd) in cases {
            let case = format!("{bash} style={style} fixed={fixed} BASH_XTRACEFD={xtracefd}");
            let _ = std::fs::remove_file(&out_file);
            let mut env = vec![
                ("ATERM_CHILD", "1"),
                ("ATERM_PROMPT_STYLE", style),
                (REKEY_PATH_VAR, rekey.as_str()),
            ];
            if xtracefd {
                env.push(("BASH_XTRACEFD", "5"));
            }
            let mut sh = spawn_pty_shell(ShellType::Bash, bash, &fx, dir, &env);
            sh.wait_for_line_editor();
            let watch = ForkWatch::new(sh.child.id());
            let mut forked = Vec::new();
            for line in plain {
                let ran = stty_runs();
                let typed = sh.type_at_prompt(format!("{line}\r").as_bytes());
                sh.wait_for_after("\x1b]133;A", typed);
                sh.wait_for_line_editor();
                if watch.forked() && stty_runs() == ran {
                    forked.push(line);
                }
            }
            if fixed {
                assert!(
                    forked.is_empty(),
                    "{case}: forked at {forked:?}\n{}",
                    sh.transcript()
                );
            } else {
                assert_eq!(
                    !forked.is_empty(),
                    level < 503,
                    "{case}: the control forked at {forked:?}\n{}",
                    sh.transcript()
                );
            }
            let ran = stty_runs();
            sh.run_line("false", "1");
            sh.wait_for_line_editor();
            assert!(
                watch.forked() && stty_runs() > ran,
                "{case}: no `stty` after `false`\n{}",
                sh.transcript()
            );
            sh.finish();
            let left = std::fs::read(&out_file);
            if fixed {
                assert_eq!(
                    left.as_deref().ok(),
                    Some(&b""[..]),
                    "{case}: the session's output file"
                );
            } else {
                assert!(left.is_err(), "{case}: the control made {out_file}");
            }
        }
    }
}

/// A CTRL-C THAT LANDS IN READLINE'S REDISPLAY LEAVES BASH 3.2 ITS PROMPT (unit BASH32H). With
/// the terminal's output stopped (`^S` under `ixon -ixany`), a recalled entry longer than the
/// terminal's output queue holds bash 3.2's readline in its write to stderr, the shell asleep with
/// no key left to read; a Ctrl-C there jumps out of bash's signal handler with stderr's stdio lock
/// held, which any subshell forked after it waits on forever (the script's `__aterm_out` section).
/// The next prompt comes, and the line after it runs with its marks, because no hook forks a
/// subshell. bash 5.3's readline acts on that Ctrl-C only at its next key, with no integration as
/// with one; the stall was measured on macOS's 3.2 alone, its terminal holding part of the entry
/// (`TIOCOUTQ`), so the rows are 3.2's on macOS, and 4.0 to 5.2 stay unmeasured. NEGATIVE
/// CONTROL, scratch: with the history read's `$(…)` back, the prompt never comes (the stall's
/// sample: the shell in a `read` of the pipe, its child in `flockfile`).
#[cfg(target_os = "macos")]
#[test]
fn a_ctrl_c_in_bash_3_2s_blocked_redisplay_leaves_it_its_next_prompt() {
    let entry = format!(": {}", "x".repeat(6000));
    let mut shells = ShellLanes::default();
    for bash in shells.bashes() {
        if bash_level(bash) >= 400 {
            continue;
        }
        let fx = LiveFixture::new();
        std::fs::write(
            fx.home.join(".bashrc"),
            format!("HISTFILE=/dev/null\ncd /\nstty ixon -ixany\nhistory -s '{entry}'\n"),
        )
        .expect(".bashrc");
        let dir = install_script_set(&fx.base).expect("install");
        let rekey = fx.rekey_path();
        let mut sh = spawn_pty_shell(
            ShellType::Bash,
            bash,
            &fx,
            &dir,
            &[("ATERM_CHILD", "1"), (REKEY_PATH_VAR, rekey.as_str())],
        );
        sh.wait_for_line_editor();
        let at = sh.len();
        sh.send_bytes(b"\x13");
        sh.send_bytes(b"\x1b[A");
        sh.wait_for_blocked_write(entry.len());
        assert!(
            !stdout_between(&sh, at, sh.len()).contains(&entry),
            "{bash}: PRECONDITION: the recalled entry is held back\n{}",
            sh.transcript()
        );
        sh.send_bytes(b"\x03");
        sh.send_bytes(b"\x11");
        sh.wait_for_after("\x1b]133;A", at);
        let typed = sh.type_at_prompt(b"echo OK$((40+2))\r");
        let ran = sh.wait_for_after("OK42", typed);
        let done = sh.wait_for_after("\x1b]133;D;0", ran);
        let end = sh.wait_for_after("\x1b]133;A", done);
        assert_eq!(
            command_marks(&stdout_between(&sh, typed, end)),
            ["E echo\\x20OK$((40+2))", "C", "D 0"],
            "{bash}: the line after the Ctrl-C\n{}",
            sh.transcript()
        );
        sh.finish();
    }
}

/// `TIOCOUTQ` from macOS's `<sys/ttycom.h>`: `_IOR('t', 115, int)`, the bytes in a terminal's
/// output queue. The libc crate exports it for Linux alone.
#[cfg(target_os = "macos")]
const TIOCOUTQ: libc::c_ulong = 0x4004_7473;

#[cfg(target_os = "macos")]
impl LiveShell {
    /// Waits until the shell sleeps in its write of `whole` bytes its stopped terminal cannot
    /// take: no key left in the terminal's input (`FIONREAD` through the terminal's node,
    /// `aterm_sysprobe`'s reading of its session leader), so it has read every key sent, and
    /// the terminal's output queue holding some of those bytes but not all (`TIOCOUTQ` through
    /// the pty's master, which shares the terminal's queues), so that sleep is its write.
    fn wait_for_blocked_write(&mut self, whole: usize) {
        use aterm_sysprobe::jobs::{JobsReader, ReadReq, RunState};
        use std::os::fd::AsRawFd as _;
        let started = std::time::Instant::now();
        let req = ReadReq {
            pid: self.child.id(),
            fds: false,
        };
        let mut reader = JobsReader::new();
        loop {
            let facts = reader.read(req);
            let seen = facts
                .as_ref()
                .map(|facts| (facts.state, facts.term.map(|word| word.queued)));
            let mut held: libc::c_int = 0;
            // SAFETY: `TIOCOUTQ` writes one `int` through its argument, `held`, which outlives
            // the call; `self.stdin` is the pty's master, open for the call's length.
            let rc = unsafe { libc::ioctl(self.stdin.as_raw_fd(), TIOCOUTQ, &raw mut held) };
            let queued = usize::try_from(held).ok().filter(|_| rc == 0);
            if seen == Some((RunState::Sleeping, Some(0)))
                && queued.is_some_and(|queued| queued > 0 && queued < whole)
            {
                return;
            }
            if let Ok(Some(status)) = self.child.try_wait() {
                panic!(
                    "{}: shell exited ({status}) before its write blocked\n{}",
                    self.label,
                    self.transcript()
                );
            }
            if started.elapsed() >= Self::TIMEOUT {
                panic!(
                    "{}: timed out waiting for its write to block (last {seen:?}, \
                     {queued:?} of {whole} bytes queued)\n{}",
                    self.label,
                    self.transcript()
                );
            }
            aterm_uds::deadline::poll_yield();
        }
    }
}

/// THE PROMPT A STYLE DRAWS COMES FROM THE HOOKS (unit BASH32H): its git segment and its prompt
/// character are values the prompt command sets, which PS1 reads by `${…}`, so drawing it forks
/// no subshell. The branch a `git` on PATH names shows, then none once it names none; the
/// character takes the error color after `false` and the separator's after `true`, alike through
/// the session's file and with none. NEGATIVE CONTROL, in the test: with the hook's call cut the
/// prompt draws neither.
#[cfg(unix)]
#[test]
fn an_integrated_bash_draws_its_styled_prompt_from_its_hooks() {
    const CUT_REV: &str = "00000000000000f1";
    const BRANCH: &str = "\x1b[38;5;3m (featx)\x1b[0m";
    const NO_BRANCH: &str = "\x1b[38;5;3m\x1b[0m";
    const OK_CHAR: &str = "\x1b[38;5;8m$\x1b[0m ";
    const ERR_CHAR: &str = "\x1b[38;5;1m$\x1b[0m ";
    let mut shells = ShellLanes::default();
    for bash in shells.bashes() {
        let fx = LiveFixture::new();
        let gitbin = fx.home.join("gitbin");
        std::fs::create_dir_all(&gitbin).expect("gitbin");
        write_executable(
            &gitbin.join("git"),
            "#!/bin/sh\n[ \"$*\" = 'rev-parse --abbrev-ref HEAD' ] || exit 2\n\
             [ -e \"$HOME/no-repo\" ] && exit 128\necho featx\n",
        );
        // In the rc: a login shell's profile (macOS's `path_helper`) puts the system's
        // folders ahead of the PATH it was handed.
        std::fs::write(
            fx.home.join(".bashrc"),
            format!(
                "HISTFILE=/dev/null\ncd /\nPATH='{}':$PATH\n",
                gitbin.display()
            ),
        )
        .expect(".bashrc");
        let ours = install_script_set(&fx.base).expect("install");
        let cut = folder_with_edits(
            &fx.base,
            CUT_REV,
            &[(
                "bash",
                scripts::BASH,
                &[(
                    "    export -n PS1\n    __aterm_prompt_segments\n",
                    "    export -n PS1\n",
                )],
            )],
        );
        let rekey = fx.rekey_path();
        for (dir, channel, fixed) in [
            (&ours, true, true),
            (&ours, false, true),
            (&cut, true, false),
        ] {
            let case = format!("{bash} channel={channel} fixed={fixed}");
            let _ = std::fs::remove_file(fx.home.join("no-repo"));
            let env = [
                ("ATERM_CHILD", "1"),
                ("ATERM_PROMPT_STYLE", "standard"),
                (REKEY_PATH_VAR, if channel { rekey.as_str() } else { "" }),
            ];
            let mut sh = spawn_pty_shell(ShellType::Bash, bash, &fx, dir, &env);
            sh.wait_for_line_editor();
            let prompt_after = |sh: &mut LiveShell, line: &str, status: &str| -> String {
                let typed = sh.type_at_prompt(format!("{line}\r").as_bytes());
                let done = sh.wait_for_after(&format!("\x1b]133;D;{status}"), typed);
                let drawn = sh.wait_for_after("\x1b]133;B", done);
                stdout_between(sh, done, drawn)
            };
            let after_true = prompt_after(&mut sh, "true", "0");
            let after_false = prompt_after(&mut sh, "false", "1");
            let no_repo = prompt_after(&mut sh, ": >~/no-repo", "0");
            if fixed {
                for (prompt, branch, char) in [
                    (&after_true, BRANCH, OK_CHAR),
                    (&after_false, BRANCH, ERR_CHAR),
                    (&no_repo, NO_BRANCH, OK_CHAR),
                ] {
                    assert!(
                        prompt.contains(&format!("{branch} {char}")),
                        "{case}: {prompt:?}\n{}",
                        sh.transcript()
                    );
                }
            } else {
                assert!(
                    !after_true.contains("featx") && !after_false.contains(ERR_CHAR),
                    "{case}: the control drew the segments: {after_true:?} {after_false:?}"
                );
            }
            sh.finish();
        }
    }
}

/// A BODY THAT DREW ITS PROMPT BY `$(…)` HANDS OVER A PROMPT THAT FORKS NONE (unit BASH32H): a
/// shell whose PS1 an older body set (`$(__aterm_git_segment)`, `$(__aterm_err_prompt)`) reads the
/// hooks' values in their place once this body loads, each occurrence, the rest of PS1 kept byte
/// for byte, and the old functions go. NEGATIVE CONTROL, scratch: without the swap PS1 keeps its
/// `$(…)`.
#[cfg(unix)]
#[test]
fn a_bash_prompt_an_older_body_set_reads_the_hooks_values_once_this_body_loads() {
    let script = format!(
        "{}/src/scripts/aterm_shell_integration.bash",
        aterm_tempfile::manifest_dir().display()
    );
    let mut shells = ShellLanes::default();
    for bash in shells.bashes() {
        let out = shell_command(bash)
            .args(["--noprofile", "--norc", "-i", "-c"])
            .arg(
                "source \"$ATERM_TEST_SCRIPT\" >/dev/null 2>&1; \
                 PS1='\\[a\\]$(__aterm_git_segment)\"$(__aterm_err_prompt)\"$(__aterm_git_segment)\\$ '; \
                 __aterm_git_segment() { :; }; __aterm_err_prompt() { :; }; \
                 source \"$ATERM_TEST_SCRIPT\" >/dev/null 2>&1; \
                 printf '<%s>' \"$PS1\"; \
                 if { declare -F __aterm_git_segment || declare -F __aterm_err_prompt; } \
                 >/dev/null 2>&1; then echo fns=kept; else echo fns=gone; fi",
            )
            .env("ATERM_TEST_SCRIPT", &script)
            .output()
            .unwrap_or_else(|error| panic!("spawn {bash}: {error}"));
        let stdout = String::from_utf8_lossy(&out.stdout);
        let ps1 = stdout
            .rsplit_once('<')
            .and_then(|(_, rest)| rest.split_once('>'))
            .map(|(ps1, _)| ps1);
        assert_eq!(
            ps1,
            Some("\\[a\\]${__aterm_git_seg-}\"${__aterm_err_ps-}\"${__aterm_git_seg-}\\$ "),
            "{bash}: {stdout:?}"
        );
        assert!(
            stdout.contains("fns=gone"),
            "{bash}: an old segment function stayed: {stdout:?}"
        );
    }
}

/// THE NEXT HOOK MENDS ONE A CTRL-C CUT SHORT (unit BASH32H): a hook that runs a program turns job
/// control off around it (`__aterm_prog`), and before 4.4 one turns xtrace on to read bash's
/// command number; a Ctrl-C in either leaves its mark up (`__aterm_jobs_held`,
/// `__aterm_trace_held`), which a line sets here as that Ctrl-C would leave it. The next line
/// has job control back and no trace, and a person's own `set +m` and `set -x`, with no mark, are
/// kept. After `false` (whose `stty` runs with job control off) it is on again. NEGATIVE
/// CONTROL, scratch: with the mend a no-op the marked lines keep job control off and the trace
/// on.
#[cfg(unix)]
#[test]
fn the_next_bash_hook_mends_one_a_ctrl_c_cut_short() {
    let mut shells = ShellLanes::default();
    for bash in shells.bashes() {
        let fx = LiveFixture::new();
        std::fs::write(fx.home.join(".bashrc"), "HISTFILE=/dev/null\ncd /\n").expect(".bashrc");
        let dir = install_script_set(&fx.base).expect("install");
        let rekey = fx.rekey_path();
        let mut sh = spawn_pty_shell(
            ShellType::Bash,
            bash,
            &fx,
            &dir,
            &[("ATERM_CHILD", "1"), (REKEY_PATH_VAR, rekey.as_str())],
        );
        sh.wait_for_line_editor();
        let flag_after = |sh: &mut LiveShell, setup: &str, status: &str, flag: char| {
            sh.run_line(setup, status);
            let from = sh.len();
            let end = sh.run_line(&format!("echo \"F=${{-//[!{flag}]/}}.\""), "0");
            sh.value_before("F=", end)
                .split('.')
                .next()
                .unwrap_or_default()
                .to_owned()
                + if stdout_between(sh, from, end).contains("+ echo") {
                    " traced"
                } else {
                    ""
                }
        };
        for (setup, status, flag, want) in [
            ("false", "1", 'm', "m"),
            ("set +m; __aterm_jobs_held=1", "0", 'm', "m"),
            ("set +m", "0", 'm', ""),
            ("set -m", "0", 'm', "m"),
            ("set -x; __aterm_trace_held=1", "0", 'x', ""),
            ("set -x", "0", 'x', "x traced"),
            ("set +x", "0", 'x', ""),
        ] {
            assert_eq!(
                flag_after(&mut sh, setup, status, flag),
                want,
                "{bash}: after `{setup}`\n{}",
                sh.transcript()
            );
        }
        sh.finish();
    }
}

/// The DEBUG trap's mend, which a control makes unconditional.
#[cfg(unix)]
const GUARDED_MEND: &str = "    [[ -n ${__aterm_hook_live-} ]] || __aterm_mend\n";

/// A HOOK'S PROGRAM UNDER FUNCTRACE LEAVES BASH ITS JOB CONTROL (unit BASH32H): with `set -T` the
/// DEBUG trap fires inside a hook too, between `__aterm_prog`'s mark and its `set +m`, and mends
/// only outside one, so the mark stands and job control comes back after the program. The
/// program is the body pointer's `rm`, which the prompt runs outside `__aterm_untraced`; the
/// pointer names the running body, so nothing reloads. NEGATIVE CONTROL, in the test: with the
/// trap's mend unguarded it takes the mark, and the shell is left with job control off.
#[cfg(unix)]
#[test]
fn a_body_pointer_read_under_functrace_leaves_bash_its_job_control() {
    const UNGUARDED_REV: &str = "00000000000000f2";
    let mut shells = ShellLanes::default();
    for bash in shells.bashes() {
        let fx = LiveFixture::new();
        std::fs::write(
            fx.home.join(".bashrc"),
            "HISTFILE=/dev/null\ncd /\nset -T\n",
        )
        .expect(".bashrc");
        let ours = install_script_set(&fx.base).expect("install");
        let unguarded = folder_with_edits(
            &fx.base,
            UNGUARDED_REV,
            &[(
                "bash",
                scripts::BASH,
                &[(GUARDED_MEND, "    __aterm_mend\n")],
            )],
        );
        let pointer = fx.home.join("integration-pointer");
        let pointer_var = pointer.to_str().expect("UTF-8");
        for (dir, guarded) in [(&ours, true), (&unguarded, false)] {
            let case = format!("{bash} guarded={guarded}");
            let rev = dir
                .file_name()
                .and_then(|name| name.to_str())
                .expect("an address");
            let mut sh = spawn_pty_shell(
                ShellType::Bash,
                bash,
                &fx,
                dir,
                &[("ATERM_CHILD", "1"), (BODY_POINTER_VAR, pointer_var)],
            );
            sh.wait_for_line_editor();
            deliver(&pointer, rev);
            sh.run_line(":", "0");
            assert!(
                !pointer.exists(),
                "{case}: PRECONDITION: the prompt took the pointer\n{}",
                sh.transcript()
            );
            let end = sh.run_line("echo \"F=${-//[!m]/}.\"", "0");
            let flags = sh.value_before("F=", end);
            assert_eq!(
                flags.split('.').next().unwrap_or_default(),
                if guarded { "m" } else { "" },
                "{case}: job control after the pointer's `rm`\n{}",
                sh.transcript()
            );
            sh.finish();
        }
    }
}
