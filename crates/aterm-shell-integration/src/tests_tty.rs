// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// ─── The tty settings a shell keeps after a raw-mode program dies ───
//
// `include!`d into lib.rs's `#[cfg(test)] mod tests` after tests_loader.rs, whose
// helpers (`LiveShell`, `LiveFixture`, `shell_command`, `stdout_between`) it uses.
//
// A raw-mode program that dies without restoring the tty (an agent SIGKILLed while it held
// the terminal, a `tty.setraw` script exiting on an error) must not leave the shell running
// every later command under `-isig -iexten -icrnl -ixon -opost`, where Ctrl-C is a literal
// ^C and Enter a bare CR that ends no line. bash restores the tty after a job that dies by
// a signal but adopts it after one that exits, so an `exit 1` breaks it too. The scripts'
// "tty settings" blocks say how they repair it.
//
// These tests drive REAL shells on a REAL pty (`script`, like the fish lanes),
// started the way aterm starts a tab — the ZDOTDIR wrapper for zsh, the
// `--rcfile` wrapper for bash — and measure the two symptoms the owner hit:
// Ctrl-C sent into a `head -c 1` waiting for input must end it (the integration's own
// `133;D;130`, the status its SIGINT gives it), and `read -r x` must end on a CR. The raw-mode program is `sh -c
// 'stty raw -echo; …'` ending by SIGKILL, `exit 1` or `exit 0` — never by an
// abort (no crash reports), and no python or node needed.
//
// NEGATIVE CONTROL, in the tests themselves: the same shells are also started
// from a folder holding THIS build's scripts with only the fix's call sites cut
// out (`unfixed_folder`), and there the same measurements must show the defect —
// ISIG off, Ctrl-C not ending `head -c 1`, CR not ending `read`. A pass is
// therefore never a property of the harness.

/// The folder name the unfixed scripts are installed under: a folder address
/// (16 lowercase hex) no build writes, so the zsh loader signs it as its revision
/// and takes a body pointer to this build's folder like any older build's shell.
#[cfg(unix)]
const UNFIXED_REV: &str = "00000000000000aa";

/// The call sites that arm the fix, per script. Cutting exactly these yields the unfixed
/// script in everything that matters here.
#[cfg(unix)]
const ZSH_TTY_CALLS: [&str; 2] = [
    "    (( __aterm_tty_armed )) || __aterm_tty_arm\n",
    "    (( __aterm_tty_frozen || last_status == 0 )) || __aterm_tty_repair\n",
];
#[cfg(unix)]
const BASH_TTY_CALLS: [&str; 1] =
    ["    [[ \"$last_status\" == 0 ]] && ! __aterm_line_ran_a_second || __aterm_tty_repair\n"];

/// How a raw-mode program ends.
#[cfg(unix)]
#[derive(Clone, Copy, Debug)]
enum RawEnd {
    Sigkill,
    Exit1,
    Exit0,
    /// `exit 0` after running 1.2 s: what a full-screen program killed from outside
    /// reports under MSYS2.
    Exit0Late,
}

#[cfg(unix)]
impl RawEnd {
    /// A child that puts the tty in raw mode, says so, and ends without
    /// restoring it. The marker's number is computed, so the echoed command
    /// line never matches it.
    fn line(self) -> &'static str {
        match self {
            Self::Sigkill => "sh -c 'stty raw -echo; echo RAW$((40+2)); kill -KILL $$'",
            Self::Exit1 => "sh -c 'stty raw -echo; echo RAW$((40+2)); exit 1'",
            Self::Exit0 => "sh -c 'stty raw -echo; echo RAW$((40+2)); exit 0'",
            Self::Exit0Late => "sh -c 'stty raw -echo; echo RAW$((40+2)); sleep 1.2; exit 0'",
        }
    }

    fn status(self) -> &'static str {
        match self {
            Self::Sigkill => "137",
            Self::Exit1 => "1",
            Self::Exit0 | Self::Exit0Late => "0",
        }
    }
}

/// The tty flags a raw mode clears, as `isig=on icrnl=off …`, read by a child
/// `stty -a` (the tty as the next command sees it).
#[cfg(unix)]
const FLAGS_LINE: &str = "__s=\" $(command stty -a | tr '\\n' ' ') \"; for __f in isig icanon iexten icrnl opost ixon; do case \"$__s\" in *\" -$__f \"*) printf '%s=off ' \"$__f\";; *) printf '%s=on ' \"$__f\";; esac; done; echo FLAGS$((40+2))";

#[cfg(unix)]
impl LiveShell {
    /// Write raw bytes to the pty (no newline added).
    fn send_bytes(&mut self, bytes: &[u8]) {
        use std::io::Write as _;
        self.stdin
            .write_all(bytes)
            .and_then(|()| self.stdin.flush())
            .unwrap_or_else(|error| {
                panic!("{}: write: {error}\n{}", self.label, self.transcript())
            });
    }

    fn len(&self) -> usize {
        self.stdout
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }

    /// Type `line` and Enter (a CR, as a terminal sends it) at the prompt
    /// ([`Self::type_at_prompt`]), then wait for the command's
    /// `133;D;<status>` and the next prompt's `133;A`. Returns the offset past
    /// that prompt mark.
    fn run_line(&mut self, line: &str, status: &str) -> usize {
        let from = self.type_at_prompt(format!("{line}\r").as_bytes());
        let done = self.wait_for_after(&format!("\x1b]133;D;{status}"), from);
        self.wait_for_after("\x1b]133;A", done)
    }

    /// Type `bytes` at the prompt the shell stands at, once its line editor
    /// reads the tty ([`Self::wait_for_line_editor`]). Returns the offset they
    /// were typed at.
    fn type_at_prompt(&mut self, bytes: &[u8]) -> usize {
        self.wait_for_line_editor();
        let from = self.len();
        self.send_bytes(bytes);
        from
    }

    /// Wait until the line editor reads the tty at the prompt the shell stands
    /// at (the last `133;A` it wrote, which the caller has waited for), and
    /// return the offset past its word for it.
    ///
    /// The prompt's `133;A` comes before the editor puts the tty in its raw,
    /// no-echo mode, and a line typed in between is echoed by the kernel as
    /// well as drawn by the editor, onto whatever the shell writes meanwhile:
    /// bash runs the user's PROMPT_COMMAND after aterm's `133;A`, and under
    /// XTRACE its `+__aterm_body_prompt_command> eval '…'` took the typed
    /// line's echo on its own line (Linux lane, aarch64 guest, bash 5.2.21: 2
    /// of 14 suite runs, 5 of 200 of that test run 8 at a time); in zsh, precmd
    /// and zle's setup come after it too, and the echo lands ahead of the
    /// prompt. Each editor writes two words only once it owns the tty:
    ///
    /// - `\e[?2004h`, bracketed paste on: readline (bash 5.1+) writes it in
    ///   `rl_prep_terminal`, after setting the tty; zle after zle-line-init,
    ///   which follows the refresh that sets zle's mode (`zrefresh` →
    ///   `zsetterm`);
    /// - aterm's `133;B`: readline draws it with the rest of PS1, after
    ///   `rl_prep_terminal`, in every bash; zle-line-init writes zsh's.
    ///
    /// Whichever comes first: bash 3.2 (macOS's /bin/bash) writes no
    /// `\e[?2004h`, and a zsh body from before the lead writes no `133;B` under
    /// an rc that replaced zle-line-init.
    fn wait_for_line_editor(&mut self) -> usize {
        const PROMPT: &[u8] = b"\x1b]133;A";
        const OWNS_THE_TTY: [&[u8]; 2] = [b"\x1b[?2004h", b"\x1b]133;B"];
        let prompt = {
            let out = self
                .stdout
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            out.windows(PROMPT.len())
                .rposition(|w| w == PROMPT)
                .map_or(0, |at| at + PROMPT.len())
        };
        let pipe = std::sync::Arc::clone(&self.stdout);
        self.wait_on(&pipe, "the line editor's \\e[?2004h or 133;B", |out| {
            let after = &out[prompt.min(out.len())..];
            OWNS_THE_TTY
                .iter()
                .filter_map(|word| find_bytes(after, word).map(|at| prompt + at + word.len()))
                .min()
        })
    }

    /// The flags `FLAGS_LINE` printed, e.g. `isig=on icanon=on … ixon=off`.
    fn flags(&mut self) -> String {
        let from = self.type_at_prompt(format!("{FLAGS_LINE}\r").as_bytes());
        let at = self.wait_for_after("FLAGS42", from);
        let text = stdout_between(self, from, at);
        let start = text
            .rfind("isig=")
            .unwrap_or_else(|| panic!("{}: no flags printed\n{}", self.label, self.transcript()));
        let flags: String = text[start..]
            .chars()
            .take_while(|c| !c.is_control())
            .collect();
        let flags = flags.trim_end_matches("FLAGS42").trim().to_owned();
        self.wait_for_after("\x1b]133;A", at);
        flags
    }

    /// Ctrl-C into a `head -c 1` waiting for input: did it END the command?
    /// Judged by the command's own status in the integration's
    /// `133;D;<status>`, never by a clock: 130 is `head` killed by the SIGINT a
    /// working ISIG makes of ^C, and a ^C the tty swallowed is a byte `head`
    /// reads, ending with 0. Leaves the shell at a prompt either way, with the
    /// line editor's buffer cleared.
    fn ctrl_c_interrupts(&mut self) -> bool {
        self.ctrl_c_status("") == "130"
    }

    /// Type `<runner>sh -c 'echo WAIT42; exec head -c 1 >/dev/null'`, send
    /// Ctrl-C and a newline once the command runs, and return its status.
    /// "Runs" is the command's OWN word: a foreground job owns the tty before it
    /// execs (both shells' children call `tcsetpgrp` first), so a ^C after
    /// `WAIT42` reaches `sh` or the `head` it became, never the shell. A fixed
    /// pause after `133;C` stood in for that and under load sent the ^C while
    /// zsh was still forking, where it interrupted nothing (`133;D;0` after `^C`).
    ///
    /// `head` ends on input alone, so no timer decides the outcome: a `sleep 4`
    /// here ran out before a ^C sent under load reached it (`133;D;0` on a
    /// healthy tty, seen at load 65), and the control waited out all 4 s. The
    /// newline is for a tty whose ISIG is off but ICANON on, where `head` gets
    /// the ^C byte only once a line ends. Where ISIG is on, the ^C's SIGINT is
    /// posted before the newline is queued, so it kills `head` whatever it
    /// reads; the newline is then an empty line at the prompt, which marks no
    /// command.
    fn ctrl_c_status(&mut self, runner: &str) -> String {
        let from = self.type_at_prompt(
            format!("{runner}sh -c 'echo WAIT$((40+2)); exec head -c 1 >/dev/null'\r").as_bytes(),
        );
        let running = self.wait_for_after("WAIT42", from);
        self.send_bytes(b"\x03\n");
        let status = self.status_after(running);
        // Ctrl-U: kill-whole-line in both line editors' default emacs bindings.
        self.send_bytes(b"\x15");
        status
    }

    /// The `<status>` of the first `133;D;<status>` after `from`, read once the
    /// next prompt's `133;A` is in (so every digit is), with the shell at it.
    fn status_after(&mut self, from: usize) -> String {
        let done = self.wait_for_after("\x1b]133;D;", from);
        self.wait_for_after("\x1b]133;A", done);
        stdout_between(self, done, (done + 3).min(self.len()))
    }

    /// `read -r x` fed `hello` + CR: did the CR end the line? Judged by what
    /// `read` got, not by a clock: an LF follows the CR, so the read ends either
    /// way — with `hello` when ICRNL made the CR a line end (the LF is then an
    /// empty line for the prompt, which marks no command), with `hello\r` when
    /// the tty passed the CR through as a byte.
    fn cr_ends_read(&mut self) -> bool {
        let from = self.type_at_prompt(b"read -r x; echo \"GOT=[$x]\"\r");
        let started = self.wait_for_after("\x1b]133;C", from);
        self.send_bytes(b"hello\r\n");
        let got = self.wait_for_after("GOT=[", started);
        let end = self.wait_for_after("]", got);
        let done = self.wait_for_after("\x1b]133;D;", end);
        self.wait_for_after("\x1b]133;A", done);
        stdout_between(self, got, end - 1) == "hello"
    }

    /// A raw-mode child ends as `end` says, and the shell returns to a prompt.
    fn kill_raw_child(&mut self, end: RawEnd) {
        let from = self.type_at_prompt(format!("{}\r", end.line()).as_bytes());
        let raw = self.wait_for_after("RAW42", from);
        let done = self.wait_for_after(&format!("\x1b]133;D;{}", end.status()), raw);
        self.wait_for_after("\x1b]133;A", done);
    }

    /// The shell's own report of its tty freeze (zsh `ttyctl`).
    fn zsh_frozen(&mut self) -> bool {
        let from = self.type_at_prompt(b"ttyctl; echo TTYCTL$((40+2))\r");
        let at = self.wait_for_after("TTYCTL42", from);
        let text = stdout_between(self, from, at);
        self.wait_for_after("\x1b]133;A", at);
        assert!(
            text.contains("tty is"),
            "{}: ttyctl said nothing\n{text}",
            self.label
        );
        !text.contains("tty is not frozen")
    }
}

/// Assert the tty is sane for the next command: the flags a raw mode clears are
/// back on (IXON as `want_ixon` says — the user's own choice), Ctrl-C ends
/// `head -c 1` and a CR ends `read`.
#[cfg(unix)]
fn assert_tty_usable(sh: &mut LiveShell, what: &str, want_ixon: bool) {
    let flags = sh.flags();
    let ixon = if want_ixon { "ixon=on" } else { "ixon=off" };
    for want in [
        "isig=on",
        "icanon=on",
        "iexten=on",
        "icrnl=on",
        "opost=on",
        ixon,
    ] {
        assert!(
            flags.split_whitespace().any(|f| f == want),
            "{}: {what}: the next command must see {want}, got `{flags}`\n{}",
            sh.label,
            sh.transcript()
        );
    }
    assert!(
        sh.ctrl_c_interrupts(),
        "{}: {what}: Ctrl-C did not end `head -c 1` (its status was not 130)\n{}",
        sh.label,
        sh.transcript()
    );
    assert!(
        sh.cr_ends_read(),
        "{}: {what}: Enter (CR) did not end `read -r x`\n{}",
        sh.label,
        sh.transcript()
    );
}

/// The defect, as the negative control must show it.
#[cfg(unix)]
fn assert_tty_broken(sh: &mut LiveShell, what: &str) {
    let flags = sh.flags();
    for want in ["isig=off", "icrnl=off"] {
        assert!(
            flags.split_whitespace().any(|f| f == want),
            "{}: {what}: the control must reproduce the defect ({want}), got `{flags}`\n{}",
            sh.label,
            sh.transcript()
        );
    }
    assert!(
        !sh.ctrl_c_interrupts(),
        "{}: {what}: the control's Ctrl-C ended `head -c 1` — the measurement cannot tell\n{}",
        sh.label,
        sh.transcript()
    );
    assert!(
        !sh.cr_ends_read(),
        "{}: {what}: the control's CR ended `read` — the measurement cannot tell\n{}",
        sh.label,
        sh.transcript()
    );
}

/// This build's scripts with the fix's call sites cut out, as a folder the
/// wrappers can start a shell from: `<root>/UNFIXED_REV`, beside this build's.
#[cfg(unix)]
fn unfixed_folder(root: &Path) -> PathBuf {
    let cut = |calls: &[&'static str]| -> Vec<(&'static str, &'static str)> {
        calls.iter().map(|call| (*call, "")).collect()
    };
    folder_with_edits(
        root,
        UNFIXED_REV,
        &[
            ("zsh", scripts::ZSH, &cut(&ZSH_TTY_CALLS)),
            ("bash", scripts::BASH, &cut(&BASH_TTY_CALLS)),
        ],
    )
}

/// One script's edits: its extension, its text, and each `(from, to)`.
#[cfg(unix)]
type ScriptEdits<'a> = (&'a str, &'a str, &'a [(&'a str, &'a str)]);

/// This build's scripts at `<root>/<rev>`, beside this build's folder, with each
/// `(from, to)` edit made to its script (and so to the body written beside it).
/// Every `from` must occur exactly once: a control that no longer differs from
/// the build would pass for the wrong reason.
#[cfg(unix)]
fn folder_with_edits(root: &Path, rev: &str, edits: &[ScriptEdits<'_>]) -> PathBuf {
    let dir = root.join(rev);
    ensure_scripts(&dir).expect("control scripts");
    for (ext, script, edits) in edits {
        let mut edited = (*script).to_owned();
        for (from, to) in *edits {
            assert_eq!(
                edited.matches(from).count(),
                1,
                "{ext}: `{}` moved — keep the control in step",
                from.trim()
            );
            edited = edited.replace(from, to);
        }
        let body = scripts::body(&edited).expect("markers").to_owned();
        std::fs::write(dir.join(format!("aterm_shell_integration.{ext}")), &edited).unwrap();
        std::fs::write(
            dir.join(format!("{BODY_FILE_STEM}.{ext}")),
            format!("{BODY_FILE_HEADER}{body}"),
        )
        .unwrap();
    }
    dir
}

/// An interactive `shell` on a real pty (`script`, macOS or util-linux
/// spelling), started from the integration folder `dir` the way aterm starts a
/// tab, in the fixture's home, with `extra` in its environment; returned at
/// its first prompt's `133;A`.
#[cfg(unix)]
fn spawn_pty_shell(
    shell: ShellType,
    program: &str,
    fx: &LiveFixture,
    dir: &Path,
    extra: &[(&str, &str)],
) -> LiveShell {
    let mut sh = spawn_pty_shell_unwaited(shell, program, fx, dir, extra);
    sh.wait_for_after("\x1b]133;A", 0);
    sh
}

/// [`spawn_pty_shell`], returned as soon as it is spawned.
#[cfg(unix)]
fn spawn_pty_shell_unwaited(
    shell: ShellType,
    program: &str,
    fx: &LiveFixture,
    dir: &Path,
    extra: &[(&str, &str)],
) -> LiveShell {
    let InjectionEnv {
        env_add,
        argv_override,
    } = injection_for(shell, dir).expect("injection");
    let mut argv: Vec<String> = vec![program.to_owned()];
    if let Some(over) = argv_override {
        argv.extend(over.into_iter().skip(1));
    }
    argv.push("-i".to_owned());
    let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
    let mut cmd = pty_script(&argv);
    // The hermetic environment of `shell_command`, applied to `script` (the
    // shell inherits it), then the fixture's home and the injection.
    let hermetic = shell_command(program);
    for (key, value) in hermetic.get_envs() {
        match value {
            Some(value) => {
                cmd.env(key, value);
            }
            None => {
                cmd.env_remove(key);
            }
        }
    }
    cmd.env("HOME", &fx.home)
        .env("PATH", "/usr/bin:/bin")
        .env("TERM", "xterm-256color");
    for (key, value) in env_add {
        cmd.env(key, value);
    }
    if shell == ShellType::Zsh {
        cmd.env_remove("ATERM_UNSET_ZDOTDIR")
            .env("ATERM_ORIGINAL_ZDOTDIR", &fx.home);
    }
    for (key, value) in extra {
        cmd.env(key, value);
    }
    let label = match shell {
        ShellType::Zsh => "zsh",
        _ => "bash",
    };
    LiveShell::spawn(label, &mut cmd).holding(hermetic.into_home())
}

/// A zsh and a pty, or COULD NOT RUN that says which is missing.
#[cfg(unix)]
fn live_pty_zsh() -> &'static str {
    let zsh = need_zsh();
    need_pty_script();
    zsh
}

/// A NEW integrated zsh: a raw-mode child dying by SIGKILL, `exit 1` or `exit 0`
/// leaves the next command's tty sane — Ctrl-C ends `head -c 1`, a CR ends `read`
/// — and the user's own `stty -ixon`, typed before, survives all three (the
/// freeze puts back exactly what the shell had). The shell reports its tty
/// frozen. The same scenario from the unfixed scripts is the negative control.
#[cfg(unix)]
#[test]
fn an_integrated_zsh_keeps_its_tty_when_a_raw_program_dies() {
    let zsh = live_pty_zsh();
    let fx = LiveFixture::new();
    std::fs::write(fx.home.join(".zshrc"), "").expect(".zshrc");
    let ours = install_script_set(&fx.base).expect("install");

    // The control: the unfixed scripts reproduce the defect.
    let unfixed = unfixed_folder(&fx.base);
    let mut sh = spawn_pty_shell(ShellType::Zsh, zsh, &fx, &unfixed, &[]);
    sh.kill_raw_child(RawEnd::Sigkill);
    assert_tty_broken(&mut sh, "unfixed, after a SIGKILLed raw child");
    sh.finish();

    let mut sh = spawn_pty_shell(ShellType::Zsh, zsh, &fx, &ours, &[]);
    assert!(sh.zsh_frozen(), "zsh: the integrated shell freezes its tty");
    for end in [RawEnd::Sigkill, RawEnd::Exit1, RawEnd::Exit0] {
        sh.kill_raw_child(end);
        assert_tty_usable(&mut sh, &format!("after a raw child's {end:?}"), true);
    }
    // A deliberate `stty -ixon` goes through the thawing wrapper and sticks —
    // from a function of the user's too, whose local `options` hides zsh's from
    // the wrapper (which must neither fail on it nor write to it).
    sh.run_line("own() { local options=mine; stty -ixon; }; own", "0");
    sh.kill_raw_child(RawEnd::Sigkill);
    assert_tty_usable(&mut sh, "stty -ixon, then a SIGKILLed raw child", false);
    sh.run_line("stty ixon", "0");
    sh.kill_raw_child(RawEnd::Exit1);
    assert_tty_usable(&mut sh, "stty ixon again, then a raw child's exit 1", true);
    sh.finish();
}

/// A NEW integrated zsh whose rc ran a raw-mode program that died: the shell
/// adopted that tty before its first prompt, whose read of it runs in the
/// background — and its first command still runs sane, repaired through the
/// thaw at that command's preexec, so the tty it then keeps frozen is the
/// repaired one (a raw child's `exit 0` after it leaves the next command sane).
/// IXON stays as the raw mode left it: the repair never touches it. The same
/// rc under the unfixed scripts is the negative control.
#[cfg(unix)]
#[test]
fn an_integrated_zsh_whose_rc_broke_the_tty_runs_its_first_command_sane() {
    let zsh = live_pty_zsh();
    let fx = LiveFixture::new();
    std::fs::write(fx.home.join(".zshrc"), "sh -c 'stty raw -echo'\n").expect(".zshrc");
    let ours = install_script_set(&fx.base).expect("install");

    let unfixed = unfixed_folder(&fx.base);
    let mut sh = spawn_pty_shell(ShellType::Zsh, zsh, &fx, &unfixed, &[]);
    assert_tty_broken(
        &mut sh,
        "unfixed, the first command after the rc's raw child",
    );
    sh.finish();

    let mut sh = spawn_pty_shell(ShellType::Zsh, zsh, &fx, &ours, &[]);
    assert_tty_usable(&mut sh, "the first command after the rc's raw child", false);
    assert!(sh.zsh_frozen(), "zsh: the repaired tty is the one frozen");
    sh.kill_raw_child(RawEnd::Exit0);
    assert_tty_usable(&mut sh, "a raw child's exit 0 after the repair", false);
    sh.finish();
}

/// The zsh line that has the line editor take a fresh shell's deferred tty
/// read the moment it answers (and repair the tty its first prompt runs in);
/// cut, the scripts are as they were before 2026-10-17 in everything that
/// matters here: the read waits for the first command.
#[cfg(unix)]
const ZSH_FIRST_INTR_CALL: &str =
    "        zle -F $__aterm_tty_probe_fd __aterm_tty_probe_ready 2>/dev/null\n";

/// The key typed right after a Ctrl-C at a zsh prompt. zsh 5.9's line editor
/// looks for an interrupt only between keys (`zlecore`'s loop, then
/// `getbyte`'s blocking `read`, zle_main.c): a SIGINT it takes after that look
/// and before the read — the ^C landing while the editor is still busy, which
/// load makes likely — ends the line only when one more byte arrives, and a
/// ^C flushes the bytes typed ahead of it. Measured 2026-10-19 under 16
/// parallel runs: a bare `zsh -i` (no aterm scripts) drew no prompt for 8 s,
/// then took the interrupt at the next byte, 2 times in 640 and 0 in 640 with
/// this key after the ^C; this test, beside its own copies and the suite, 15
/// in 480, sampled in `getbyte`'s `read` with ISIG on. The kernel's flush for
/// the ^C runs before the key is queued, so the key ends the dying line there,
/// or leads the next line as a harmless space where the interrupt already
/// took the line. A self-insert in both keymaps, so a ^C that is only a byte
/// keeps the line.
#[cfg(unix)]
const ZLE_INTR_WAKE: &[u8] = b" ";

/// A NEW integrated zsh whose rc left the tty raw (`stty raw -isig`): a
/// Ctrl-C at its FIRST prompt — no command yet, so the deferred read is not
/// taken at a preexec — drops the line it was typed on, as a working Ctrl-C
/// does (`$?` 130), and the tty is repaired: in the emacs keymap and in viins.
/// Once the read answers the line editor redraws the prompt on the repaired
/// tty, which is what the test waits for before it types. The ^C waits for
/// the typed line to be drawn and is followed by a space, as a user types on
/// (both explained at the ^C). The same rc without the editor's watch on the
/// read is the negative control: the ^C is a byte the editor beeps at, and
/// the line survives into the next command.
#[cfg(unix)]
#[test]
fn an_integrated_zsh_whose_rc_left_the_tty_raw_takes_ctrl_c_at_its_first_prompt() {
    let zsh = live_pty_zsh();
    let fx = LiveFixture::new();
    let ours = install_script_set(&fx.base).expect("install");
    let unfixed = folder_with_edits(
        &fx.base,
        "00000000000000ab",
        &[("zsh", scripts::ZSH, &[(ZSH_FIRST_INTR_CALL, "")])],
    );
    let raw_rc = "PS1='@@P@@ '\nstty raw -isig\n";
    for (dir, rc, fixed) in [
        (&unfixed, raw_rc.to_owned(), false),
        (&ours, raw_rc.to_owned(), true),
        (&ours, format!("bindkey -v\n{raw_rc}"), true),
    ] {
        std::fs::write(fx.home.join(".zshrc"), &rc).expect(".zshrc");
        let mut sh = spawn_pty_shell(ShellType::Zsh, zsh, &fx, dir, &[]);
        let at = sh.wait_for_line_editor();
        if fixed {
            // The prompt drawn again, on the repaired tty.
            sh.wait_for_after("@@P@@", at);
        }
        // The markers are computed, so the echoed lines never match them.
        let typed = sh.len();
        sh.send_bytes(b"echo LO$((7*7))");
        // The ^C goes in once the editor has drawn the line to its end, so has
        // read all of it and has nothing more to write. A ^C flushes the tty's
        // output too, and `script` sets its pty master non-blocking and stops
        // relaying for good when a read of it finds nothing (macOS shell_cmds
        // script.c: `if (cc <= 0) break;`, then `finish()` waits for the
        // shell): a ^C that lands with the editor's echo in flight empties the
        // queue between `script`'s select and its read, and the shell then
        // waits forever in `tcsetattr(TCSADRAIN)` for an output nobody drains
        // (sampled 2026-10-19 with the space alone, 1 in 480 beside the suite:
        // zsh in `hend` → `settyinfo`, `script` in `wait4`).
        let from = sh.wait_for_after("))", typed);
        sh.send_bytes(b"\x03");
        sh.send_bytes(ZLE_INTR_WAKE);
        if fixed {
            // An interrupt flushes the tty's input: type on only at the next
            // prompt. The control's ^C is a byte, and there is none.
            sh.wait_for_after("\x1b]133;A", from);
            sh.wait_for_line_editor();
        }
        let from = sh.len();
        sh.send_bytes(b"echo KEPT$((40+2)) $?\r");
        let kept = sh.wait_for_after("KEPT42", from);
        let ran = cut_osc(&stdout_between(&sh, from, kept));
        // What the command printed ahead of the marker, on the marker's line.
        let line = ran
            .strip_suffix("KEPT42")
            .and_then(|r| r.rsplit(['\n', '\r']).next())
            .unwrap_or_default()
            .to_owned();
        let done = sh.wait_for_after("\x1b]133;D;", kept);
        let status = cut_osc(&stdout_between(&sh, kept, done));
        sh.wait_for_after("\x1b]133;A", done);
        if fixed {
            assert_eq!(
                (line.as_str(), status.split_whitespace().next()),
                ("", Some("130")),
                "zsh ({rc:?}): the first prompt's Ctrl-C dropped its line as an \
                 interrupt does; the next command printed `{line}KEPT42{status}`\n{}",
                sh.transcript()
            );
            assert_tty_usable(&mut sh, "after the first prompt's Ctrl-C", false);
        } else {
            assert_eq!(
                line,
                "LO49 echo ",
                "zsh: the control must keep the line the inert ^C was typed on\n{}",
                sh.transcript()
            );
        }
        sh.finish();
    }
}

/// A user whose rc defines `stty` as a FUNCTION (or `reset` as an ALIAS) keeps
/// zsh's adopting behaviour (a frozen tty would silently undo what their
/// definition does): the tty is not frozen, and the repair still runs after a
/// FAILED command — a raw child's SIGKILL and `exit 1` leave the next command
/// sane, `stty -ixon` included.
#[cfg(unix)]
#[test]
fn an_integrated_zsh_with_its_own_stty_is_repaired_not_frozen() {
    let zsh = live_pty_zsh();
    let fx = LiveFixture::new();
    std::fs::write(fx.home.join(".zshrc"), "stty() { command stty \"$@\"; }\n").expect(".zshrc");
    let ours = install_script_set(&fx.base).expect("install");
    let mut sh = spawn_pty_shell(ShellType::Zsh, zsh, &fx, &ours, &[]);
    assert!(
        !sh.zsh_frozen(),
        "zsh: a user's own `stty` function rules the freeze out"
    );
    sh.run_line("stty -ixon", "0");
    for end in [RawEnd::Sigkill, RawEnd::Exit1] {
        sh.kill_raw_child(end);
        assert_tty_usable(&mut sh, &format!("own stty, a raw child's {end:?}"), false);
    }
    sh.finish();

    // An alias for one of the three names rules it out the same way.
    std::fs::write(fx.home.join(".zshrc"), "alias reset='command reset'\n").expect(".zshrc");
    let mut sh = spawn_pty_shell(ShellType::Zsh, zsh, &fx, &ours, &[]);
    assert!(
        !sh.zsh_frozen(),
        "zsh: a user's own `reset` alias rules the freeze out"
    );
    sh.kill_raw_child(RawEnd::Sigkill);
    // The repair cannot tell a raw mode's IXON-off from the user's, so it leaves
    // IXON as the dead program left it (off) — only the freeze puts it back.
    assert_tty_usable(&mut sh, "own reset alias, a SIGKILLed raw child", false);
    sh.finish();
}

/// A running zsh with a broken tty, from the unfixed scripts, gets the fix through the
/// loader channel: a SIGKILLed raw child breaks it (the negative control); the host points
/// it at this build's folder, and at its next prompt the new body repairs the tty it had
/// adopted and freezes it, so the next command and the next raw child's death leave Ctrl-C
/// and Enter working.
#[cfg(unix)]
#[test]
fn a_running_zsh_with_a_broken_tty_is_repaired_by_the_body_the_loader_delivers() {
    let zsh = live_pty_zsh();
    let fx = LiveFixture::new();
    std::fs::write(fx.home.join(".zshrc"), "").expect(".zshrc");
    let ours = install_script_set(&fx.base).expect("install");
    let ours_rev = ours
        .file_name()
        .and_then(|n| n.to_str())
        .expect("an address")
        .to_owned();
    let unfixed = unfixed_folder(&fx.base);
    let pointer = fx.home.join("integration-pointer");
    let pointer_env = pointer.to_str().expect("UTF-8").to_owned();
    let mut sh = spawn_pty_shell(
        ShellType::Zsh,
        zsh,
        &fx,
        &unfixed,
        &[(BODY_POINTER_VAR, pointer_env.as_str())],
    );
    assert!(!sh.zsh_frozen(), "zsh: the unfixed body freezes nothing");
    sh.kill_raw_child(RawEnd::Sigkill);
    assert_tty_broken(&mut sh, "unfixed, after a SIGKILLed raw child");

    // The host points the running shell at this build's body.
    deliver(&pointer, &ours_rev);
    let at = sh.run_line("true", "0");
    let signed = format!("633;P;{INTEGRATION_REV_KEY}={ours_rev}");
    assert!(
        stdout_between(&sh, 0, at).contains(&signed),
        "zsh: the running shell took this build's body\n{}",
        sh.transcript()
    );
    assert!(!pointer.exists(), "zsh: the pointer was consumed");
    assert_tty_usable(&mut sh, "the delivered body's first prompt", false);
    assert!(
        sh.zsh_frozen(),
        "zsh: the delivered body froze the repaired tty"
    );
    sh.kill_raw_child(RawEnd::Exit1);
    assert_tty_usable(&mut sh, "the next raw child's exit 1", false);
    sh.finish();
}

/// bash: after a raw child's `exit 1` (which bash adopts — it restores the tty
/// only after a SIGNAL death), and after an `exit 0` from one that ran a second,
/// the next command's tty is sane, and a SIGKILL stays fine; a deliberate `stty
/// -ixon` survives. The unfixed scripts are the negative control, on the `exit 1`
/// that breaks them.
#[cfg(unix)]
#[test]
fn an_integrated_bash_repairs_its_tty_after_a_raw_program_fails() {
    need_pty_script();
    let bash = bash_shell();
    let fx = LiveFixture::new();
    std::fs::write(fx.home.join(".bashrc"), "").expect(".bashrc");
    let ours = install_script_set(&fx.base).expect("install");

    let unfixed = unfixed_folder(&fx.base);
    let mut sh = spawn_pty_shell(ShellType::Bash, bash, &fx, &unfixed, &[]);
    sh.kill_raw_child(RawEnd::Exit1);
    assert_tty_broken(&mut sh, "unfixed, after a raw child's exit 1");
    sh.finish();

    let mut sh = spawn_pty_shell(ShellType::Bash, bash, &fx, &ours, &[]);
    // bash's own restore after a signal death puts IXON back too; the repair
    // after an `exit 1` cannot tell a raw mode's IXON-off from the user's and
    // leaves it off (which bash then keeps, as the state it saved).
    for (end, ixon) in [
        (RawEnd::Sigkill, true),
        (RawEnd::Exit1, false),
        (RawEnd::Exit0Late, false),
    ] {
        sh.kill_raw_child(end);
        assert_tty_usable(&mut sh, &format!("after a raw child's {end:?}"), ixon);
    }
    sh.run_line("stty ixon", "0");
    sh.run_line("stty -ixon", "0");
    sh.kill_raw_child(RawEnd::Exit1);
    assert_tty_usable(&mut sh, "stty -ixon, then a raw child's exit 1", false);
    sh.finish();
}

/// Under `setopt err_return`, a failing wrapped command (`stty bogusflag`) must not return
/// from `__aterm_tty_thaw_run` before its `ttyctl -f`, which would leave the shell thawed
/// while `__aterm_tty_frozen` says 1 (switching the fallback repair off too). The wrapper
/// opens with `emulate -L zsh` and re-freezes in an `always` block. The failing status
/// still reaches the prompt (`133;D;1`), the tty reports frozen, and a raw child dying by
/// `exit 1` and by `exit 0` (which no fallback repair catches) leaves the next command sane.
#[cfg(unix)]
#[test]
fn an_integrated_zsh_with_err_return_stays_frozen_after_a_failing_stty() {
    let zsh = live_pty_zsh();
    let fx = LiveFixture::new();
    std::fs::write(fx.home.join(".zshrc"), "setopt err_return\n").expect(".zshrc");
    let ours = install_script_set(&fx.base).expect("install");
    let mut sh = spawn_pty_shell(ShellType::Zsh, zsh, &fx, &ours, &[]);
    assert!(
        sh.zsh_frozen(),
        "zsh: err_return: the integrated shell freezes its tty"
    );
    // A typo'd `stty` fails; its status is the wrapper's status.
    sh.run_line("stty bogusflag", "1");
    assert!(
        sh.zsh_frozen(),
        "zsh: err_return: a FAILING wrapped `stty` must leave the tty frozen\n{}",
        sh.transcript()
    );
    for end in [RawEnd::Exit1, RawEnd::Exit0] {
        sh.kill_raw_child(end);
        assert_tty_usable(
            &mut sh,
            &format!("err_return, a failing stty, then a raw child's {end:?}"),
            true,
        );
    }
    sh.finish();
}

/// A thawed command INTERRUPTED by Ctrl-C: zsh
/// abandons the rest of a function when its foreground child dies by SIGINT, so
/// a `ttyctl -f` written after the command never ran — and Ctrl-C at `tset`'s
/// "TERM = (unknown)?" question, or during `reset`'s pause, is the ordinary way
/// that happens. The re-freeze is in an `always` block now, which runs on that
/// path too. Driven through `__aterm_tty_thaw_run` with a `head -c 1` that ends
/// only on the ^C, so the timing is the test's, not a terminal database's; the
/// shell must report its tty frozen afterwards, and a raw child's `exit 0` must
/// then leave the next command sane.
#[cfg(unix)]
#[test]
fn an_integrated_zsh_refreezes_when_a_thawed_command_is_interrupted() {
    let zsh = live_pty_zsh();
    let fx = LiveFixture::new();
    std::fs::write(fx.home.join(".zshrc"), "").expect(".zshrc");
    let ours = install_script_set(&fx.base).expect("install");
    let mut sh = spawn_pty_shell(ShellType::Zsh, zsh, &fx, &ours, &[]);
    assert!(sh.zsh_frozen(), "zsh: the integrated shell freezes its tty");
    let status = sh.ctrl_c_status("__aterm_tty_thaw_run ");
    assert_eq!(
        status,
        "130",
        "zsh: Ctrl-C ended the thawed `head -c 1`\n{}",
        sh.transcript()
    );
    assert!(
        sh.zsh_frozen(),
        "zsh: an INTERRUPTED thawed command must leave the tty frozen\n{}",
        sh.transcript()
    );
    sh.kill_raw_child(RawEnd::Exit0);
    assert_tty_usable(
        &mut sh,
        "an interrupted thaw, then a raw child's exit 0",
        true,
    );
    sh.finish();
}

// ─── 133;B whatever the rc does to zle-line-init ───

/// The folder of a body from before the lead (`__aterm_line_init_lead`, which puts 133;B in
/// front of whatever holds zle-line-init; 16 lowercase hex no build writes): what a rollback,
/// or an older build adopting the shell, points it at.
#[cfg(unix)]
const BEFORE_LEAD_REV: &str = "00000000000000bb";

/// That body's zle-line-init, defined last in this build's body: no per-prompt
/// re-assertion, and the loader's widget marks every line it is reached on.
#[cfg(unix)]
const BEFORE_LEAD_ZSH: &str = "\
__aterm_line_init_wire() { :; }
__aterm_zle_line_init() {
    __aterm_mark_command_start
    (( ${+widgets[__aterm_orig_zle_line_init]} )) && zle __aterm_orig_zle_line_init
}
";

/// Every prompt of an integrated zsh carries one 133;B whatever its ~/.zshrc does to
/// zle-line-init after the loader wired it (the engine takes C only after a B, so a prompt
/// without one keeps no block or exit code), and every widget the rc put there still runs
/// once per line, through a body switch to the build before (a rollback) and back. The rc
/// runs after the loader (zsh reads it from the ZDOTDIR .zshenv). The rcs:
///
/// - one that replaces zle-line-init (oh-my-zsh's `echoti smkx` widget), then wraps it
///   again from a precmd hook at the first prompt (powerlevel10k over oh-my-zsh): the
///   replacing widget still runs and reads in `$?` the status of the command before the
///   line, as in bare zsh;
/// - one that wraps it (powerlevel10k alone): the line is marked once and never loops;
/// - one that wraps it again from a precmd hook whenever its wrapper lost the front: the
///   wrapper runs once;
/// - one that hooks it with add-zle-hook-widget, at load and from a precmd hook: the
///   dispatcher stops at the first hook that fails, and the loader's widget is that first
///   hook, so it succeeds (the build before's returns 1, so after a switch to it the user's
///   hooks do not run); one 133;B however often the hook is added;
/// - one that wraps it under a new name each time it lost the front (zsh-autosuggestions):
///   each line runs one wrapper, not every wrapper so far;
/// - one that reads the widget table to choose between wrapping and replacing (zsh-vi-mode
///   at its first precmd, powerlevel10k while it expands its first prompt): the widget it
///   replaces never runs, as in bare zsh;
/// - one that registers through add-zle-hook-widget where the dispatcher leads and wraps
///   otherwise, with a hook added from a later precmd: no dispatcher-wrapper loop;
/// - one whose zle-line-init returns non-zero, which a user's ZERR trap sees once, from
///   that widget;
/// - one whose precmd hook fails with an error, after which zsh runs no due `sched` event
///   until zle reads the line.
///
/// Every rc also sets PRINT_EXIT_VALUE (a failed command's status is reported once, below
/// the command), WARN_NESTED_VAR and WARN_CREATE_GLOBAL (aterm's functions that run with
/// the user's options give nothing to warn about), a ZERR trap (fires for the failed
/// command, never inside aterm's precmd) and VERBOSE (aterm's own code never reaches the
/// screen; each typed line is echoed once).
///
/// After a switch to the build before, a line is marked where that build's widget is
/// reached, at most twice (the engine ignores a B outside a prompt). The build before is
/// also the negative control: from its folder, the replacing rc's prompts carry no 133;B.
#[cfg(unix)]
#[test]
fn an_integrated_zsh_marks_every_prompt_whatever_its_rc_does_to_zle_line_init() {
    let zsh = live_pty_zsh();
    let fx = LiveFixture::new();
    let ours = install_script_set(&fx.base).expect("install");
    let ours_rev = ours
        .file_name()
        .and_then(|n| n.to_str())
        .expect("an address")
        .to_owned();
    let before_body_end = format!("{BEFORE_LEAD_ZSH}{}", scripts::BODY_END);
    let before = folder_with_edits(
        &fx.base,
        BEFORE_LEAD_REV,
        &[(
            "zsh",
            scripts::ZSH,
            &[(scripts::BODY_END, before_body_end.as_str())],
        )],
    );
    let pointer = fx.home.join("integration-pointer");
    let pointer_env = pointer.to_str().expect("UTF-8").to_owned();
    let replaces = "autoload -Uz add-zsh-hook\n\
        function zle-line-init() { print -rn -- \"OWN$((40+2))?$?;\" }\n\
        zle -N zle-line-init\n\
        _rc_wrap() { zle _rc_wrapped; print -n WRAP$((40+2)) }\n\
        _rc_wrap_late() {\n\
          add-zsh-hook -d precmd _rc_wrap_late\n\
          zle -A zle-line-init _rc_wrapped\n\
          zle -N zle-line-init _rc_wrap\n\
        }\n\
        add-zsh-hook precmd _rc_wrap_late\n";
    let wraps = "zle -A zle-line-init _rc_wrapped\n\
        _rc_wrap() { zle _rc_wrapped; print -n WRAP$((40+2)) }\n\
        zle -N zle-line-init _rc_wrap\n";
    let rewraps = "autoload -Uz add-zsh-hook\n\
        _rc_again() { zle _rc_again_orig; print -n AGAIN$((40+2)) }\n\
        _rc_again_wire() {\n\
          [[ ${widgets[zle-line-init]} == user:_rc_again ]] && return\n\
          zle -A zle-line-init _rc_again_orig\n\
          zle -N zle-line-init _rc_again\n\
        }\n\
        add-zsh-hook precmd _rc_again_wire\n";
    let hooks = "autoload -Uz add-zle-hook-widget add-zsh-hook\n\
        _rc_hook() { print -n HOOK$((40+2)) }\n\
        zle -N _rc_hook\n\
        add-zle-hook-widget line-init _rc_hook\n\
        _rc_late() { print -n LATE$((40+2)) }\n\
        zle -N _rc_late\n\
        _rc_late_wire() { add-zle-hook-widget line-init _rc_late }\n\
        add-zsh-hook precmd _rc_late_wire\n";
    let renames = "autoload -Uz add-zsh-hook\n\
        typeset -gi _rc_n=0\n\
        _rc_rename_wire() {\n\
          [[ ${widgets[zle-line-init]-} == user:_rc_named_* ]] && return\n\
          (( _rc_n++ ))\n\
          (( ${+widgets[zle-line-init]} )) && zle -A zle-line-init _rc_named_orig_$_rc_n\n\
          functions[_rc_named_$_rc_n]=\"zle _rc_named_orig_$_rc_n; print -n RENAMED\\$((40+2))\"\n\
          zle -N zle-line-init _rc_named_$_rc_n\n\
        }\n\
        add-zsh-hook precmd _rc_rename_wire\n";
    let reads = "autoload -Uz add-zsh-hook\n\
        function zle-line-init() { print -n OWN$((40+2)) }\n\
        zle -N zle-line-init\n\
        _rc_vi() { print -n VI$((40+2)) }\n\
        _rc_vi_init() {\n\
          add-zsh-hook -d precmd _rc_vi_init\n\
          local -a w=($(zle -l -L zle-line-init))\n\
          if (( $#w == 4 )); then\n\
            functions[_rc_vi_wrap]=\"$w[4]; _rc_vi\"\n\
            zle -N zle-line-init _rc_vi_wrap\n\
          else\n\
            zle -N zle-line-init _rc_vi\n\
          fi\n\
        }\n\
        add-zsh-hook precmd _rc_vi_init\n\
        _rc_exp() { zle ._rc_exp_orig; print -n EXP$((40+2)) }\n\
        _rc_expand() {\n\
          (( ${+widgets[._rc_exp_orig]} )) && return 0\n\
          zle -A zle-line-init ._rc_exp_orig\n\
          zle -N zle-line-init _rc_exp\n\
        }\n\
        functions -M _rc_expand 0 0\n\
        setopt prompt_subst\n\
        PS1='${${:-$(( _rc_expand() ))}:#*}%# '\n";
    let refuses = "function zle-line-init() { print -n OWN$((40+2)); return 1 }\n\
        zle -N zle-line-init\n";
    let fails = "autoload -Uz add-zsh-hook\n\
        function zle-line-init() { print -n OWN$((40+2)) }\n\
        zle -N zle-line-init\n\
        _rc_fail() { : ${_rc_unset:?} }\n\
        add-zsh-hook precmd _rc_fail\n";
    let dispatched = "autoload -Uz add-zle-hook-widget add-zsh-hook\n\
        function zle-line-init() { print -n OWN$((40+2)) }\n\
        zle -N zle-line-init\n\
        _rc_hook() { print -n HOOK$((40+2)) }\n\
        zle -N _rc_hook\n\
        add-zle-hook-widget line-init _rc_hook\n\
        _rc_exp() { print -n EXP$((40+2)) }\n\
        _rc_exp_wrap() { zle ._rc_exp_orig; _rc_exp }\n\
        _rc_expand() {\n\
          (( ${+widgets[_rc_exp]} )) && return 0\n\
          zle -N _rc_exp\n\
          if [[ ${widgets[zle-line-init]} == user:azhw:zle-line-init ]]; then\n\
            add-zle-hook-widget line-init _rc_exp\n\
          else\n\
            zle -A zle-line-init ._rc_exp_orig\n\
            zle -N zle-line-init _rc_exp_wrap\n\
          fi\n\
        }\n\
        functions -M _rc_expand 0 0\n\
        setopt prompt_subst\n\
        PS1='${${:-$(( _rc_expand() ))}:#*}%# '\n\
        _rc_late() { print -n LATE$((40+2)) }\n\
        zle -N _rc_late\n\
        typeset -gi _rc_n=0\n\
        _rc_late_wire() {\n\
          (( ++_rc_n == 2 )) || return 0\n\
          add-zsh-hook -d precmd _rc_late_wire\n\
          add-zle-hook-widget line-init _rc_late\n\
        }\n\
        add-zsh-hook precmd _rc_late_wire\n";
    // `n` prompts, each from its 133;A to the next one's: its line-init, then a
    // command, whose status cycles through 3, 1 and 0 (each form of the event: a
    // prompt after a 3 is checked in every phase), and whether zsh reports that
    // status (`zsh: exit <status>`, PRINT_EXIT_VALUE). `run_line` types each
    // line once zle owns the tty, so its only echo is VERBOSE's: typed at the
    // bare 133;A, a loaded machine lets the tty echo it ahead of the prompt too
    // (`\n(exit 3)\r\n` twice in one prompt).
    let lines = [("(exit 3)", "3"), ("false", "1"), ("true", "0")];
    let prompts = |sh: &mut LiveShell, at: &mut usize, n: usize| -> Vec<(String, usize, &str)> {
        (0..n)
            .map(|i| {
                let (line, status) = lines[i % lines.len()];
                let next = sh.run_line(line, status);
                let prompt = stdout_between(sh, *at, next);
                *at = next;
                (prompt, usize::from(status != "0"), line)
            })
            .collect()
    };

    // The options and ZERR trap every rc runs under; the trap names the function the
    // failed command ran in.
    let rc_file = |rc: &str| {
        std::fs::write(
            fx.home.join(".zshrc"),
            format!(
                "setopt print_exit_value warn_nested_var warn_create_global verbose\n\
                 TRAPZERR() {{ print -rn -- \"<ZERR:${{funcstack[2]-}}>\" }}\n{rc}"
            ),
        )
        .expect(".zshrc");
    };

    // The control: the build before marks no prompt of the replacing rc.
    rc_file(replaces);
    let mut sh = spawn_pty_shell(ShellType::Zsh, zsh, &fx, &before, &[]);
    let mut at = sh.wait_for_after("\x1b]133;A", 0);
    for (prompt, _, _) in prompts(&mut sh, &mut at, 2) {
        assert!(
            !prompt.contains("\x1b]133;B") && prompt.contains("OWN42"),
            "zsh: the control must reproduce the defect\n{}",
            sh.transcript()
        );
    }
    sh.finish();

    // The rc's own widgets and how often each runs in a line, as in bare zsh:
    // from this build, and after a switch to the build before.
    for (what, rc, own, own_before) in [
        (
            "an rc that replaces zle-line-init",
            replaces,
            &[("OWN42", 1), ("WRAP42", 1)][..],
            &[("OWN42", 1), ("WRAP42", 1)][..],
        ),
        (
            "an rc that wraps zle-line-init",
            wraps,
            &[("WRAP42", 1)][..],
            &[("WRAP42", 1)][..],
        ),
        (
            "an rc that wraps zle-line-init again at every prompt",
            rewraps,
            &[("AGAIN42", 1)][..],
            &[("AGAIN42", 1)][..],
        ),
        (
            "an rc that hooks zle-line-init with add-zle-hook-widget",
            hooks,
            &[("HOOK42", 1), ("LATE42", 1)][..],
            &[][..],
        ),
        (
            "an rc that wraps zle-line-init under a new name whenever it lost the front",
            renames,
            &[("RENAMED42", 1)][..],
            &[("RENAMED42", 1)][..],
        ),
        (
            "an rc that reads zle-line-init to choose between wrapping and replacing",
            reads,
            &[("VI42", 1), ("EXP42", 1), ("OWN42", 0)][..],
            &[("VI42", 1), ("EXP42", 1), ("OWN42", 0)][..],
        ),
        (
            "an rc whose zle-line-init returns non-zero",
            refuses,
            &[("OWN42", 1), ("<ZERR:zle-line-init>", 1)][..],
            &[("OWN42", 1), ("<ZERR:zle-line-init>", 1)][..],
        ),
        (
            "an rc whose precmd hook fails with an error",
            fails,
            &[("OWN42", 1)][..],
            &[("OWN42", 1)][..],
        ),
        (
            "an rc that registers with add-zle-hook-widget where its dispatcher leads",
            dispatched,
            &[("OWN42", 1), ("HOOK42", 1), ("EXP42", 1)][..],
            &[("OWN42", 1), ("HOOK42", 1)][..],
        ),
    ] {
        rc_file(rc);
        let mut sh = spawn_pty_shell(
            ShellType::Zsh,
            zsh,
            &fx,
            &ours,
            &[(BODY_POINTER_VAR, pointer_env.as_str())],
        );
        let check = |sh: &LiveShell,
                     phase: &str,
                     prompts: &[(String, usize, &str)],
                     marks: std::ops::RangeInclusive<usize>,
                     own: &[(&str, usize)]| {
            for (n, (prompt, reports, line)) in prompts.iter().enumerate() {
                let b = prompt.matches("\x1b]133;B").count();
                assert!(
                    marks.contains(&b),
                    "zsh, {what}, {phase}, prompt {}: {b} 133;B\n{prompt:?}\n{}",
                    n + 1,
                    sh.transcript()
                );
                for (marker, times) in own {
                    assert_eq!(
                        prompt.matches(marker).count(),
                        *times,
                        "zsh, {what}, {phase}, prompt {}: the rc's {marker} widget runs {times} times\n{prompt:?}",
                        n + 1
                    );
                }
                assert_eq!(
                    prompt.matches("zsh: exit").count(),
                    *reports,
                    "zsh, {what}, {phase}, prompt {}: the status is reported once, below its command\n{prompt:?}",
                    n + 1
                );
                assert!(
                    !prompt.contains("__aterm"),
                    "zsh, {what}, {phase}, prompt {}: aterm's own code on the screen (a warning, a ZERR trap, a VERBOSE echo)\n{prompt:?}",
                    n + 1
                );
                assert_eq!(
                    prompt.matches(&format!("\n{line}\r\n")).count(),
                    1,
                    "zsh, {what}, {phase}, prompt {}: VERBOSE echoes the typed line once\n{prompt:?}",
                    n + 1
                );
                assert!(
                    !prompt.contains("nested function level")
                        && !prompt.contains("recursion limit"),
                    "zsh, {what}, {phase}: no widget loop\n{prompt:?}"
                );
                // A widget that prints its `$?` (`OWN42?<status>;`) reads the
                // status of the line before this prompt's.
                let before = n.checked_sub(1).map(|b| prompts[b].2);
                let want = lines.iter().find(|(l, _)| Some(*l) == before);
                if let (true, Some((_, status))) = (prompt.contains("OWN42?"), want) {
                    assert!(
                        prompt.contains(&format!("OWN42?{status};")),
                        "zsh, {what}, {phase}, prompt {}: the rc's zle-line-init reads `$?` = {status}\n{prompt:?}",
                        n + 1
                    );
                }
            }
        };
        let mut at = sh.wait_for_after("\x1b]133;A", 0);
        let fresh = prompts(&mut sh, &mut at, 3);
        check(&sh, "a new shell", &fresh, 1..=1, own);
        // A body switch is taken at the prompt after the pointer is written.
        for (phase, rev, marks, own) in [
            (
                "after a switch to the build before",
                BEFORE_LEAD_REV,
                0..=2,
                own_before,
            ),
            ("after a switch back", ours_rev.as_str(), 1..=1, own),
        ] {
            deliver(&pointer, rev);
            for (prompt, _, _) in prompts(&mut sh, &mut at, 1) {
                assert!(
                    !prompt.contains("__aterm"),
                    "zsh, {what}, {phase}: the body taken at the switch echoed on the screen\n{prompt:?}"
                );
            }
            let switched = prompts(&mut sh, &mut at, 2);
            check(&sh, phase, &switched, marks, own);
        }
        sh.finish();
    }
}

/// A user's XTRACE traces the user's commands, not aterm's, and never the capability
/// nonce (bare zsh traces nothing between two commands). XTRACE comes from `.zshenv` here,
/// so the load runs under it too, and the rc replaces zle-line-init (oh-my-zsh's shape), so
/// the scheduled event runs: its own line, which zsh prints before it runs, is all of
/// aterm's that is left, `stty`'s wrapper included, as bare zsh traces only the line that
/// ran it.
#[cfg(unix)]
#[test]
fn an_integrated_zsh_under_xtrace_traces_the_users_commands_and_not_aterms() {
    let zsh = live_pty_zsh();
    let fx = LiveFixture::new();
    let dir = install_script_set(&fx.base).expect("install");
    std::fs::write(fx.home.join(".zshenv"), "setopt xtrace\n").expect(".zshenv");
    std::fs::write(
        fx.home.join(".zshrc"),
        "function zle-line-init() { : }\nzle -N zle-line-init\n",
    )
    .expect(".zshrc");
    let mut sh = spawn_pty_shell(
        ShellType::Zsh,
        zsh,
        &fx,
        &dir,
        &[("ATERM_SHELL_NONCE", REKEY_OLD), ("ATERM_CHILD", "1")],
    );
    for (line, status) in [
        ("(exit 3)", "3"),
        ("false", "1"),
        ("echo traced$((40+2))", "0"),
        ("stty -a >/dev/null", "0"),
    ] {
        sh.run_line(line, status);
    }
    let text = cut_osc(&stdout_between(&sh, 0, sh.len()));
    assert!(
        text.contains("> echo traced42"),
        "zsh: the user's own command is traced\n{text}"
    );
    assert!(
        !text.contains(REKEY_OLD),
        "zsh: the capability nonce reached the trace\n{text}"
    );
    // The event's own line names `__aterm_line_init_late` at the top level
    // (`+zsh:<n>> …`); a line traced inside a function of aterm's carries its
    // name as PS4's `%N` (`+__aterm_…:<n>> …`), that function's included.
    for line in text.lines().filter(|line| line.contains("__aterm")) {
        assert!(
            line.contains("__aterm_line_init_late") && !line.contains("+__aterm"),
            "zsh: aterm's code in the trace: {line}\n{text}"
        );
    }
    sh.finish();
}

/// A shell's text with every OSC frame cut: what reads on the screen, where the
/// marks carry the key by design.
#[cfg(unix)]
fn cut_osc(raw: &str) -> String {
    let mut text = String::new();
    let mut rest = raw;
    while let Some(at) = rest.find("\x1b]") {
        text.push_str(&rest[..at]);
        rest = rest[at..]
            .find('\x07')
            .map_or("", |end| &rest[at + end + 1..]);
    }
    text.push_str(rest);
    text
}

/// bash's twin of the test above: a user's XTRACE traces the user's commands and their
/// PROMPT_COMMAND, not aterm's, and never the capability nonce. XTRACE comes from
/// `.bashrc`, so the load runs under it too, and PS4 names the function a line runs in:
/// what is left of aterm's is bash's own trace of the two hooks' calls, at the top level,
/// and the user's PROMPT_COMMAND, which aterm's prompt handler runs.
#[cfg(unix)]
#[test]
fn an_integrated_bash_under_xtrace_traces_the_users_commands_and_not_aterms() {
    need_pty_script();
    let fx = LiveFixture::new();
    let dir = install_script_set(&fx.base).expect("install");
    std::fs::write(
        fx.home.join(".bashrc"),
        "PS4='+${FUNCNAME[0]-}> '\nset -x\nPROMPT_COMMAND='echo pc$((40+2))'\n",
    )
    .expect(".bashrc");
    let mut sh = spawn_pty_shell(
        ShellType::Bash,
        bash_shell(),
        &fx,
        &dir,
        &[("ATERM_SHELL_NONCE", REKEY_OLD), ("ATERM_CHILD", "1")],
    );
    for (line, status) in [("false", "1"), ("echo traced$((40+2))", "0"), ("cd /", "0")] {
        sh.run_line(line, status);
    }
    let text = cut_osc(&stdout_between(&sh, 0, sh.len()));
    assert!(
        !text.contains(REKEY_OLD),
        "bash: the capability nonce reached the trace\n{text}"
    );
    // (the function a traced line runs in, the line). Readline in bash 5.1+
    // ends each accepted line with `\e[?2004l\r` (bracketed paste off), and
    // the command's trace follows it on the same line: a line is what its
    // last `\r` left on the screen.
    let traced: Vec<(&str, &str)> = text
        .lines()
        .filter_map(|line| line.rsplit('\r').next()?.strip_prefix('+'))
        .filter_map(|line| line.trim_start_matches('+').split_once("> "))
        .map(|(frame, line)| (frame, line.trim_end()))
        .collect();
    for line in ["echo traced42", "cd /", "echo pc42"] {
        assert!(
            traced.iter().any(|(_, traced)| *traced == line),
            "bash: the user's `{line}` is traced\n{text}"
        );
    }
    for (frame, line) in &traced {
        let users = if frame.starts_with("__aterm") {
            matches!(*line, "eval 'echo pc$((40+2))'" | "echo pc42")
        } else {
            // The hooks' own calls: the DEBUG trap's, PROMPT_COMMAND's and, on
            // bash 5.3+, PS0's.
            !line.contains("__aterm")
                || matches!(
                    line.split_whitespace().next(),
                    Some("__aterm_preexec" | "__aterm_prompt_command" | "__aterm_ps0")
                )
        };
        assert!(
            users,
            "bash: aterm's code in the trace: +{frame}> {line}\n{text}"
        );
    }
    sh.finish();
}

/// A user's ERR trap, `set -e` and `set -o functrace` meet the commands bare bash shows
/// them. aterm's PROMPT_COMMAND runs no failing command of its own (a returned status, a
/// `( exit <status> )`, a `hash -d` of a name not hashed, a reroute derivation that says
/// no, a missing atpkg hook's open): each would fire the ERR trap again at the prompt, or
/// close a `set -e` shell after a failure bash lets pass (`false && true`). Under
/// functrace the DEBUG trap does not fire inside aterm's prompt functions (it would report
/// one of their lines as the user's command and drop the typed command's 633;E); without
/// functrace, a function wrapping the trap (starship's layering on bash < 4.4) still
/// reports the commands typed after it. The user's PROMPT_COMMAND still reads `$?`.
#[cfg(unix)]
#[test]
fn an_integrated_bash_keeps_the_users_err_trap_errexit_and_functrace_as_bare_bash_does() {
    need_pty_script();
    let fx = LiveFixture::new();
    let dir = install_script_set(&fx.base).expect("install");
    // atpkg's dirs, its hook not yet written: a tab of a machine with atpkg.
    fx.lay_dirs();
    std::fs::write(
        fx.home.join(".bashrc"),
        "trap 'echo err$((40+2))' ERR\nPROMPT_COMMAND='echo \"pc=$?\"'\n",
    )
    .expect(".bashrc");
    let mut sh = spawn_pty_shell(
        ShellType::Bash,
        bash_shell(),
        &fx,
        &dir,
        &[
            ("ATERM_CHILD", "1"),
            ("ATPKG_AGENTS", fx.agents.to_str().expect("UTF-8")),
        ],
    );
    let from = sh.len();
    sh.run_line("false", "1");
    sh.run_line("echo next$((40+2))", "0");
    let upto = sh.wait_for_after("next42", from);
    let text = stdout_between(&sh, from, upto);
    assert_eq!(
        text.matches("err42").count(),
        1,
        "bash: the ERR trap fires once for one `false`\n{}",
        sh.transcript()
    );
    assert!(
        text.contains("pc=1"),
        "bash: the user's PROMPT_COMMAND reads the command's `$?`\n{}",
        sh.transcript()
    );
    sh.run_line("set -e", "0");
    sh.run_line("false && true", "1");
    sh.run_line("ls -d / >/dev/null", "0");
    // atpkg writes its hook, as on every install and update.
    fx.lay_hook("bash", ATPKG_HOOK_POSIX_GOLDEN);
    sh.run_line("echo alive$((40+2))", "0");
    let from = sh.run_line("set -o functrace", "0");
    // An empty line: a prompt with no command before it.
    sh.type_at_prompt(b"\r");
    sh.wait_for_after("\x1b]133;A", from);
    sh.run_line("f() { echo in-f; }; f", "0");
    sh.run_line("echo hi", "0");
    let reported = |text: String| -> Vec<String> {
        text.split("\x1b]633;E;")
            .skip(1)
            .map(|rest| rest.split([';', '\x07']).next().unwrap_or("").to_owned())
            .collect()
    };
    assert_eq!(
        reported(stdout_between(&sh, from, sh.len())),
        [
            "f()\\x20{\\x20echo\\x20in-f\\x3b\\x20}\\x3b\\x20f",
            "echo\\x20hi"
        ],
        "bash: the commands 633;E reported under functrace\n{}",
        stdout_between(&sh, from, sh.len())
    );
    sh.run_line("set +o functrace", "0");
    // starship's layering when it finds a DEBUG trap (bash < 4.4), typed.
    let from = sh.run_line(
        r#"w() { local a=$1; eval -- "$T"; : "$a"; }; eval "T=($(trap -p DEBUG))"; T=("${T[2]}"); trap 'w "$_"' DEBUG"#,
        "0",
    );
    sh.type_at_prompt(b"echo wrapped$((40+2))\r");
    let at = sh.wait_for_after("wrapped42", from);
    let end = sh.wait_for_after("\x1b]133;A", at);
    assert_eq!(
        reported(stdout_between(&sh, from, end)),
        ["echo\\x20wrapped$((40+2))"],
        "bash: the command 633;E reported behind a function wrapping the trap\n{}",
        stdout_between(&sh, from, end)
    );
    sh.finish();
}

/// THE RC'S DEBUG TRAP (2026-10-17): a DEBUG trap the user's rc sets — two
/// lines, a single quote inside — runs, chained, for a typed command, and
/// aterm's own preexec still marks that command (`133;C`): with a re-key path,
/// where the wrapper hands the trap over in `<re-key path>.trap` (which then
/// holds it as `trap -p` prints it — no fork), and without one. NEGATIVE
/// CONTROL: the same scripts with the wrapper's hand-over cut, where the
/// integration's own `trap -p DEBUG` inside the `.` sees no trap: bash 3.2
/// puts the rc's trap back over aterm's when the `.` returns (no mark), bash
/// 4.4 and later keep aterm's and drop the rc's (no output) — never both. The
/// line is one bash does not save (ignorespace), which only the DEBUG trap
/// marks: bash 5.3's PS0 marks a saved line whatever the trap, so a saved line
/// showed nothing of a chained trap that stopped marking.
#[cfg(unix)]
#[test]
fn an_integrated_bash_chains_the_debug_trap_its_rc_set() {
    need_pty_script();
    let bash = bash_shell();
    let fx = LiveFixture::new();
    std::fs::write(
        fx.home.join(".bashrc"),
        "HISTCONTROL=ignorespace\n\
         trap '[[ $BASH_COMMAND != \"echo typed\"* ]] ||\n  echo \"DBG'\\''$((40+2))\"' DEBUG\n",
    )
    .expect(".bashrc");
    let ours = install_script_set(&fx.base).expect("install");
    let unhanded = folder_with_edits(&fx.base, "00000000000000ac", &[]);
    // The loader this platform starts bash on: `bash/bashrc` off macOS.
    let rcfile = unhanded.join("bash").join(BASH_LOADER_FILE);
    let wrapper = std::fs::read_to_string(&rcfile).expect("the control's loader");
    assert_eq!(
        wrapper.matches(bash_debug_trap_handover!()).count(),
        1,
        "the hand-over moved — keep the control in step"
    );
    std::fs::write(&rcfile, wrapper.replace(bash_debug_trap_handover!(), "")).expect("rcfile");
    let rekey_dir = fx.base.join("rekey");
    std::fs::create_dir_all(&rekey_dir).expect("re-key dir");
    let sid = "s-0123456789abcdef0123";
    let rekey = rekey_dir.join(sid);
    let trap_file = rekey_dir.join(format!("{sid}{DEBUG_TRAP_SUFFIX}"));
    let rekey = rekey.to_str().expect("UTF-8");
    for (dir, channel, fixed) in [
        (&unhanded, true, false),
        (&ours, true, true),
        (&ours, false, true),
    ] {
        let _ = std::fs::remove_file(&trap_file);
        let extra: &[(&str, &str)] = if channel {
            &[(REKEY_PATH_VAR, rekey)]
        } else {
            &[]
        };
        let mut sh = spawn_pty_shell(ShellType::Bash, bash, &fx, dir, extra);
        let from = sh.type_at_prompt(b" echo typed$((6*7))\r");
        let ran = sh.wait_for_after("typed42", from);
        let done = sh.wait_for_after("\x1b]133;A", ran);
        let text = stdout_between(&sh, from, done);
        let (chained, marked) = (text.contains("DBG'42"), text.contains("\x1b]133;C"));
        if fixed {
            assert!(
                chained && marked,
                "bash (channel {channel}): the rc's DEBUG trap ran (chained {chained}) and \
                 aterm marked the command (marked {marked})\n{}",
                sh.transcript()
            );
            let handed = std::fs::read_to_string(&trap_file).ok();
            assert_eq!(
                handed.as_deref(),
                channel.then_some(
                    "trap -- '[[ $BASH_COMMAND != \"echo typed\"* ]] ||\n  echo \"DBG'\\''$((40+2))\"' DEBUG\n"
                ),
                "bash (channel {channel}): the wrapper hands the trap over in the session \
                 file exactly when there is one"
            );
        } else {
            assert!(
                !(chained && marked),
                "bash: the control must lose the rc's trap or aterm's mark\n{}",
                sh.transcript()
            );
        }
        sh.finish();
    }
}

/// The guard on the wrapper's DEBUG-trap hand-over: only a fresh load takes it.
#[cfg(unix)]
const BASH_HANDOVER_GUARD: &str = "if [ -z \"${ATERM_SHELL_INTEGRATION_INSTALLED:-}\" ]; then\n";

/// A `.bashrc` that sources the integration itself (the line its header gives
/// for a shell aterm did not start) keeps aterm's command marks in an aterm
/// tab, with a re-key path and without: that load wired the DEBUG trap, and
/// the wrapper's own load is then an upgrade in place that wires nothing, so
/// the wrapper hands nothing over. NEGATIVE CONTROL: the hand-over unguarded,
/// as 195c01197d wrote it, clears the trap the rc's load set, and the typed
/// command goes unmarked. The line is one bash does not save (ignorespace),
/// which only the DEBUG trap marks: bash 5.3's PS0 marks a saved line whatever
/// the trap, so a saved line showed the control nothing there.
#[cfg(unix)]
#[test]
fn an_integrated_bash_whose_rc_sources_the_integration_keeps_its_marks() {
    need_pty_script();
    let bash = bash_shell();
    let fx = LiveFixture::new();
    std::fs::write(
        fx.home.join(".bashrc"),
        "HISTCONTROL=ignorespace\n\
         . \"$ATERM_SHELL_INTEGRATION_DIR/aterm_shell_integration.bash\"\n",
    )
    .expect(".bashrc");
    let ours = install_script_set(&fx.base).expect("install");
    let unguarded = folder_with_edits(&fx.base, "00000000000000ad", &[]);
    // The loader this platform starts bash on: `bash/bashrc` off macOS.
    let rcfile = unguarded.join("bash").join(BASH_LOADER_FILE);
    let wrapper = std::fs::read_to_string(&rcfile).expect("the control's loader");
    assert_eq!(
        wrapper.matches(BASH_HANDOVER_GUARD).count(),
        1,
        "the guard moved — keep the control in step"
    );
    std::fs::write(
        &rcfile,
        wrapper.replace(BASH_HANDOVER_GUARD, "if :; then\n"),
    )
    .expect("rcfile");
    let rekey_dir = fx.base.join("rekey");
    std::fs::create_dir_all(&rekey_dir).expect("re-key dir");
    let rekey = rekey_dir.join("s-0123456789abcdef0124");
    let rekey = rekey.to_str().expect("UTF-8");
    for (dir, channel, fixed) in [
        (&unguarded, true, false),
        (&ours, true, true),
        (&ours, false, true),
    ] {
        let extra: &[(&str, &str)] = if channel {
            &[(REKEY_PATH_VAR, rekey)]
        } else {
            &[]
        };
        let mut sh = spawn_pty_shell(ShellType::Bash, bash, &fx, dir, extra);
        let from = sh.type_at_prompt(b" echo typed$((6*7))\r");
        let ran = sh.wait_for_after("typed42", from);
        let done = sh.wait_for_after("\x1b]133;A", ran);
        let marked = stdout_between(&sh, from, done).contains("\x1b]133;C");
        assert_eq!(
            marked,
            fixed,
            "bash (channel {channel}, guarded {fixed}): aterm marked the typed command\n{}",
            sh.transcript()
        );
        sh.finish();
    }
}

/// The integration's read of the handed-over DEBUG trap (`read -d ''`, which
/// answers 1 at the file's end) is no failed command to a `.bashrc`'s ERR trap
/// or its `set -e`: the shell reaches its first prompt with nothing of the
/// load reported, chains the rc's DEBUG trap and runs what is typed.
#[cfg(unix)]
#[test]
fn an_integrated_bash_whose_rc_set_errexit_and_an_err_trap_loads_quietly() {
    need_pty_script();
    let bash = bash_shell();
    let fx = LiveFixture::new();
    std::fs::write(
        fx.home.join(".bashrc"),
        "trap 'echo ERR$((40+2))' ERR\nset -e\n\
         trap '[[ $BASH_COMMAND != \"echo typed\"* ]] || echo DBG$((40+2))' DEBUG\n",
    )
    .expect(".bashrc");
    let dir = install_script_set(&fx.base).expect("install");
    let rekey_dir = fx.base.join("rekey");
    std::fs::create_dir_all(&rekey_dir).expect("re-key dir");
    let rekey = rekey_dir.join("s-0123456789abcdef0125");
    let mut sh = spawn_pty_shell(
        ShellType::Bash,
        bash,
        &fx,
        &dir,
        &[(REKEY_PATH_VAR, rekey.to_str().expect("UTF-8"))],
    );
    let loaded = sh.type_at_prompt(b"echo typed$((6*7))\r");
    let ran = sh.wait_for_after("typed42", loaded);
    sh.wait_for_after("\x1b]133;A", ran);
    let load = stdout_between(&sh, 0, loaded);
    assert!(
        !load.contains("ERR42"),
        "bash: the load reported a failed command to the rc's ERR trap\n{}",
        sh.transcript()
    );
    assert!(
        stdout_between(&sh, loaded, ran).contains("DBG42"),
        "bash: the rc's DEBUG trap chained\n{}",
        sh.transcript()
    );
    sh.finish();
}

/// The block's command and the tab title are the LINE as typed — a list, a
/// pipeline, an alias — where they were its first simple command, alias-expanded
/// (`true && sleep 3 | cat` showed `true` while it ran, `ll` was `ls -alF`). A
/// line bash did not save (ignorespace) is reported as before, never as the
/// previous line, and a user PROMPT_COMMAND that rewrites the history (`history
/// -a; history -c; history -r`) does not make a saved line look unsaved.
#[cfg(unix)]
#[test]
fn an_integrated_bash_reports_the_line_as_typed() {
    need_pty_script();
    let fx = LiveFixture::new();
    let dir = install_script_set(&fx.base).expect("install");
    std::fs::write(
        fx.home.join(".bashrc"),
        "alias ll='ls -alF'\nHISTCONTROL=ignorespace\n\
         PROMPT_COMMAND='history -a; history -c; history -r'\n",
    )
    .expect(".bashrc");
    let mut sh = spawn_pty_shell(
        ShellType::Bash,
        bash_shell(),
        &fx,
        &dir,
        &[("ATERM_CHILD", "1")],
    );
    let reported = |text: &str| -> Vec<String> {
        text.split("\x1b]633;E;")
            .skip(1)
            .map(|rest| rest.split([';', '\x07']).next().unwrap_or("").to_owned())
            .collect()
    };
    let from = sh.len();
    sh.run_line("true && echo a$((40+2))", "0");
    sh.run_line("echo b | cat", "0");
    sh.run_line("echo c; (exit 7)", "7");
    sh.run_line("ll -d / >/dev/null", "0");
    let end = sh.run_line(" echo hidden && true", "0");
    let text = stdout_between(&sh, from, end);
    assert_eq!(
        reported(&text),
        [
            "true\\x20&&\\x20echo\\x20a$((40+2))",
            "echo\\x20b\\x20|\\x20cat",
            "echo\\x20c\\x3b\\x20(exit\\x207)",
            "ll\\x20-d\\x20/\\x20>/dev/null",
            "echo\\x20hidden",
        ],
        "bash: the lines 633;E reported\n{}",
        sh.transcript()
    );
    assert!(
        text.contains("\x1b]0;true && echo a$((40+2))\x07"),
        "bash: the title while the line runs is the line\n{}",
        sh.transcript()
    );
    sh.finish();
}

/// An ARRAY PROMPT_COMMAND (bash 5.1+ runs every element, aterm's first): the
/// user's elements are no typed line, and the line typed after them is judged
/// against the history as they left it. An element holding several commands
/// (`__x=1; __y=2`) made a block at every prompt, an empty Enter's too, and a
/// `history -a; history -c; history -r` element that read another tab's line
/// made an ignorespace line typed next be reported, and titled, as that line.
#[cfg(unix)]
#[test]
fn an_integrated_bash_with_an_array_prompt_command_reports_only_typed_lines() {
    need_pty_script();
    let fx = LiveFixture::new();
    let dir = install_script_set(&fx.base).expect("install");
    let history = fx.home.join(".bash_history");
    std::fs::write(
        fx.home.join(".bashrc"),
        "HISTFILE=~/.bash_history\nHISTCONTROL=ignoreboth\n\
         PROMPT_COMMAND=('__x=1; __y=2' 'history -a; history -c; history -r')\n",
    )
    .expect(".bashrc");
    let mut sh = spawn_pty_shell(
        ShellType::Bash,
        bash_shell(),
        &fx,
        &dir,
        &[("ATERM_CHILD", "1")],
    );
    let reported = |text: &str| -> Vec<String> {
        text.split("\x1b]633;E;")
            .skip(1)
            .map(|rest| rest.split([';', '\x07']).next().unwrap_or("").to_owned())
            .collect()
    };
    // Another tab's line, as its own `history -a` writes it.
    let other_tab_appends = || {
        use std::io::Write as _;
        std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(&history)
            .and_then(|mut file| file.write_all(b"echo OTHER-TAB-LINE\n"))
            .expect("append to the history file");
    };
    let enter = |sh: &mut LiveShell| {
        let at = sh.type_at_prompt(b"\r");
        sh.wait_for_after("\x1b]133;A", at)
    };
    let from = sh.len();
    sh.run_line("echo a$((40+2)) && true", "0");
    enter(&mut sh);
    other_tab_appends();
    sh.run_line(" echo hidden && true", "0");
    other_tab_appends();
    enter(&mut sh);
    sh.run_line(" echo hidden2 && true", "0");
    let end = sh.run_line("echo z | cat", "0");
    let text = stdout_between(&sh, from, end);
    assert_eq!(
        reported(&text),
        [
            "echo\\x20a$((40+2))\\x20&&\\x20true",
            "echo\\x20hidden",
            "echo\\x20hidden2",
            "echo\\x20z\\x20|\\x20cat",
        ],
        "bash: the lines 633;E reported\n{}",
        sh.transcript()
    );
    assert!(
        !text.contains("OTHER-TAB-LINE"),
        "bash: another tab's line reported or titled\n{}",
        sh.transcript()
    );
    sh.finish();
}

/// The user's `$_` reaches the user's code as in bare bash: `mkdir -p d && cd
/// "$_"` enters d, a command reads the last argument of the one before it, and so
/// does the user's PROMPT_COMMAND. The DEBUG trap's own command is the last one
/// bash runs before each of the user's, and called bare it left `$_` reading
/// `__aterm_preexec` (`cd: __aterm_preexec: No such file or directory`); the
/// user's PROMPT_COMMAND read the last argument of a command of aterm's.
#[cfg(unix)]
#[test]
fn an_integrated_bash_keeps_the_users_last_argument() {
    need_pty_script();
    let fx = LiveFixture::new();
    let dir = install_script_set(&fx.base).expect("install");
    std::fs::write(
        fx.home.join(".bashrc"),
        "PROMPT_COMMAND='echo \"pc_=[$_]\"'\n",
    )
    .expect(".bashrc");
    let mut sh = spawn_pty_shell(
        ShellType::Bash,
        bash_shell(),
        &fx,
        &dir,
        &[("ATERM_CHILD", "1")],
    );
    sh.run_line(
        "cd \"$HOME\" && mkdir -p last42 && cd \"$_\"; echo \"in=${PWD##*/}\"",
        "0",
    );
    sh.run_line("echo one last$((40+1))", "0");
    let end = sh.run_line("echo \"was=[$_]\"", "0");
    let text = stdout_between(&sh, 0, end);
    for said in ["in=last42", "pc_=[last41]", "was=[last41]"] {
        assert!(text.contains(said), "bash: {said}\n{}", sh.transcript());
    }
    sh.finish();
}

/// A compound command typed with its stdout redirected leaves the file holding
/// its output and nothing else, and its block on the terminal. The DEBUG trap
/// fires for the first simple command inside the compound once bash has applied
/// the compound's redirections, so the preexec's 633;E (the line and the
/// capability nonce), running title and 133;C went into the user's file ahead of
/// its data, and the block never started on the terminal (measured on bash
/// 5.2.21 in a real aterm: 419 bytes in `f` for `{ echo …; echo …; } > f`). The
/// shapes: a group, `for`, `if`, `while` with a here-string, stdout and stderr
/// both on the file (the marks then take /dev/tty: `script` gives the shell a
/// controlling terminal), and stdout moved for the shell itself (`exec >f`),
/// which put every mark of every prompt in the file. This build's script with
/// the writer's terminal test cut is the negative control. On bash 5.3+ PS0
/// starts a line bash saved before the compound's redirections apply, and the
/// DEBUG trap starts only a line bash did not save (ignorespace): the control
/// types those, and this build's script both.
#[cfg(unix)]
#[test]
fn an_integrated_bash_keeps_its_marks_out_of_a_redirected_compound_command() {
    need_pty_script();
    // (the line, the file it writes, what the file must hold)
    let compounds = [
        ("{ echo a; } > out-group", "out-group", "a\n"),
        (
            "for i in 1 2; do echo for-$i; done > out-for",
            "out-for",
            "for-1\nfor-2\n",
        ),
        (
            "if true; then echo if-body; fi > out-if",
            "out-if",
            "if-body\n",
        ),
        (
            "while read -r l; do echo got-$l; done <<< x > out-while",
            "out-while",
            "got-x\n",
        ),
        ("{ echo both; } > out-both 2>&1", "out-both", "both\n"),
    ];
    let fx = LiveFixture::new();
    std::fs::write(
        fx.home.join(".bashrc"),
        "HISTCONTROL=ignorespace\ncd \"$HOME\"\n",
    )
    .expect(".bashrc");
    let nonce = [("ATERM_SHELL_NONCE", REKEY_OLD), ("ATERM_CHILD", "1")];
    let read = |file: &str| {
        let path = fx.home.join(file);
        let text =
            std::fs::read(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
        String::from_utf8_lossy(&text).into_owned()
    };

    let unfixed = folder_with_edits(
        &fx.base,
        UNFIXED_REV,
        &[(
            "bash",
            scripts::BASH,
            &[(
                "    if [[ -t 1 || ${__aterm_osc_tty-} != 1 ]]; then\n",
                "    if :; then\n",
            )],
        )],
    );
    let mut sh = spawn_pty_shell(ShellType::Bash, bash_shell(), &fx, &unfixed, &nonce);
    for (line, file, _) in compounds {
        sh.run_line(&format!(" {line}"), "0");
        assert!(
            read(file).contains("\x1b]133;C"),
            "unfixed: ` {line}` should show the defect, its marks in {file}: {:?}\n{}",
            read(file),
            sh.transcript()
        );
    }
    sh.finish();

    let ours = install_script_set(&fx.base).expect("install");
    let mut sh = spawn_pty_shell(ShellType::Bash, bash_shell(), &fx, &ours, &nonce);
    let block = |sh: &mut LiveShell, line: &str| {
        let from = sh.len();
        let end = sh.run_line(line, "0");
        let text = stdout_between(sh, from, end);
        let typed = line
            .replace('\\', "\\\\")
            .replace(';', "\\x3b")
            .replace(' ', "\\x20");
        let mut at = 0;
        for mark in [
            format!("\x1b]633;E;{typed};id={REKEY_OLD}\x07"),
            format!("\x1b]0;{line}\x07"),
            format!("\x1b]133;C;id={REKEY_OLD}\x07"),
            format!("\x1b]133;D;0;id={REKEY_OLD}\x07"),
        ] {
            let Some(found) = text[at..].find(&mark) else {
                panic!(
                    "bash: `{line}`: {mark:?} did not reach the terminal after the marks before it\n{}",
                    sh.transcript()
                );
            };
            at += found + mark.len();
        }
    };
    for (line, file, want) in compounds {
        let from = sh.len();
        let end = sh.run_line(&format!(" {line}"), "0");
        assert_eq!(read(file), want, "bash: ` {line}` wrote into {file}");
        assert!(
            stdout_between(&sh, from, end).contains(&format!("\x1b]133;C;id={REKEY_OLD}\x07")),
            "bash: ` {line}`: its 133;C did not reach the terminal\n{}",
            sh.transcript()
        );
        block(&mut sh, line);
        assert_eq!(read(file), want, "bash: `{line}` wrote into {file}");
    }
    block(&mut sh, "exec 3>&1 >out-exec");
    block(&mut sh, "echo in-log");
    block(&mut sh, "exec >&3 3>&-");
    assert_eq!(read("out-exec"), "in-log\n", "bash: `exec >out-exec`");
    sh.finish();
}

/// zsh's `133;B` says it stands where the buffer starts (`aterm_input=start`)
/// only over an EMPTY buffer. zle draws a line pre-filled from its buffer stack
/// before zle-line-init runs, and the mark comes from zle-line-init: after that
/// text, where aterm-core must not take it for the buffer's start
/// (`Terminal::input_region_start`). Here `print -z` pushes `echo pushed`, and
/// the next prompt pops it: its signed `B` comes after the popped text and
/// does not say so, while the prompt before it, over an empty buffer, does
/// (the NEGATIVE CONTROL of the same shell).
#[cfg(unix)]
#[test]
fn an_integrated_zsh_says_its_b_starts_the_buffer_only_over_an_empty_one() {
    let zsh = live_pty_zsh();
    let fx = LiveFixture::new();
    let dir = install_script_set(&fx.base).expect("install");
    std::fs::write(fx.home.join(".zshrc"), "").expect(".zshrc");
    let mut sh = spawn_pty_shell(
        ShellType::Zsh,
        zsh,
        &fx,
        &dir,
        &[("ATERM_SHELL_NONCE", REKEY_OLD), ("ATERM_CHILD", "1")],
    );
    let signed_b = format!("\x1b]133;B;id={REKEY_OLD}");
    // The mark through its terminator, from the offset where it starts.
    let mark_at = |sh: &mut LiveShell, from: usize| -> String {
        let end = sh.wait_for_after(&signed_b, from);
        let close = sh.wait_for_after("\x07", end);
        stdout_between(sh, end - signed_b.len(), close)
    };
    let empty = mark_at(&mut sh, 0);
    assert_eq!(
        empty,
        format!("{signed_b};aterm_input=start\x07"),
        "zsh: over an empty buffer the B says so\n{}",
        sh.transcript()
    );
    let next = sh.run_line("print -z echo pushed", "0");
    let prefilled = mark_at(&mut sh, next);
    assert_eq!(
        prefilled,
        format!("{signed_b}\x07"),
        "zsh: after a pre-filled line the B says nothing of the buffer's start\n{}",
        sh.transcript()
    );
    let drawn = stdout_between(&sh, next, sh.len());
    let (text, mark) = (drawn.find("echo pushed"), drawn.find(&signed_b));
    assert!(
        matches!((text, mark), (Some(t), Some(m)) if t < m),
        "zsh: the popped line is drawn before the mark\n{drawn:?}"
    );
    // Ctrl-U takes the popped line back before `exit`.
    sh.send_bytes(b"\x15");
    sh.finish();
}
