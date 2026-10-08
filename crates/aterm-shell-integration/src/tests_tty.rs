// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// ─── The tty settings a shell keeps after a raw-mode program dies ───
//
// `include!`d into lib.rs's `#[cfg(test)] mod tests` after tests_loader.rs, whose
// helpers (`LiveShell`, `LiveFixture`, `shell_command`, `stdout_between`) it uses.
//
// THE INCIDENT. A raw-mode program that dies without restoring the tty — Claude
// Code SIGKILLed while it held the terminal (measured on a real Claude Code
// 2.1.283 in a headless aterm, 2026-09-26), or any `tty.setraw` script that exits
// on an error — left the integrated zsh running every later command under
// `-isig -iexten -icrnl -ixon -opost`: Ctrl-C arrived as a literal ^C and
// interrupted nothing, and Enter sent a bare CR that ended no line (`read x`
// returned `hello\r`, and only on Ctrl-J). bash puts the tty back after a job that
// dies by a SIGNAL, but adopts it after one that EXITS, so an `exit 1` broke it
// the same way. The scripts' "tty settings" blocks carry the whole story.
//
// These tests drive REAL shells on a REAL pty (`script`, like the fish lanes),
// started the way aterm starts a tab — the ZDOTDIR wrapper for zsh, the
// `--rcfile` wrapper for bash — and measure the two symptoms the owner hit:
// Ctrl-C sent into `sleep 4` must end it (the integration's own `133;D;130`, the
// status its SIGINT gives it), and `read -r x` must end on a CR. The raw-mode program is `sh -c
// 'stty raw -echo; …'` ending by SIGKILL, `exit 1` or `exit 0` — never by an
// abort (no crash reports), and no python or node needed.
//
// NEGATIVE CONTROL, in the tests themselves: the same shells are also started
// from a folder holding THIS build's scripts with only the fix's call sites cut
// out (`unfixed_folder`), and there the same measurements must show the defect —
// ISIG off, Ctrl-C not ending `sleep 4`, CR not ending `read`. A pass is
// therefore never a property of the harness.

/// The folder name the unfixed scripts are installed under: a folder address
/// (16 lowercase hex) no build writes, so the zsh loader signs it as its revision
/// and takes a body pointer to this build's folder like any older build's shell.
#[cfg(unix)]
const UNFIXED_REV: &str = "00000000000000aa";

/// The call sites that arm the fix, per script. Cutting exactly these yields the
/// script as it was before 2026-09-26 in everything that matters here.
#[cfg(unix)]
const ZSH_TTY_CALLS: [&str; 2] = [
    "    (( __aterm_tty_armed )) || __aterm_tty_arm\n",
    "    (( __aterm_tty_frozen || last_status == 0 )) || __aterm_tty_repair\n",
];
#[cfg(unix)]
const BASH_TTY_CALLS: [&str; 1] = ["    [[ \"$last_status\" == 0 ]] || __aterm_tty_repair\n"];

/// How a raw-mode program ends.
#[cfg(unix)]
#[derive(Clone, Copy, Debug)]
enum RawEnd {
    Sigkill,
    Exit1,
    Exit0,
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
        }
    }

    fn status(self) -> &'static str {
        match self {
            Self::Sigkill => "137",
            Self::Exit1 => "1",
            Self::Exit0 => "0",
        }
    }
}

/// The tty flags a raw mode clears, as `isig=on icrnl=off …`, read by a child
/// `stty -a` (the tty as the NEXT command sees it).
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

    /// Type `line` and Enter (a CR, as a terminal sends it), then wait for the
    /// command's `133;D;<status>` and the next prompt's `133;A`. Returns the
    /// offset past that prompt mark.
    fn run_line(&mut self, line: &str, status: &str) -> usize {
        let from = self.len();
        self.send_bytes(format!("{line}\r").as_bytes());
        let done = self.wait_for_after(&format!("\x1b]133;D;{status}"), from);
        self.wait_for_after("\x1b]133;A", done)
    }

    /// Wait until zsh's line editor owns the tty for the prompt whose `133;A`
    /// ends at `at`: the `\e[?2004h` (bracketed paste on) zle writes after its
    /// first refresh, the refresh that puts the tty in zle's raw, no-echo mode
    /// (`zrefresh` → `zsetterm`). A line typed before that lands in a tty that
    /// still echoes, so the kernel echoes it ahead of the prompt as well as zle
    /// drawing it on the line; `133;A` comes from precmd, well before. Returns
    /// the offset past the mark.
    fn wait_for_zle(&mut self, at: usize) -> usize {
        self.wait_for_after("\x1b[?2004h", at)
    }

    /// The flags `FLAGS_LINE` printed, e.g. `isig=on icanon=on … ixon=off`.
    fn flags(&mut self) -> String {
        let from = self.len();
        self.send_bytes(format!("{FLAGS_LINE}\r").as_bytes());
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

    /// Ctrl-C into a running `sleep 4`: did it END the sleep? Judged by the
    /// command's own status in the integration's `133;D;<status>`, not by a clock:
    /// 130 is `sleep` killed by the SIGINT a working ISIG makes of ^C, and a ^C the
    /// tty swallowed as a byte lets it run out with 0. Leaves the shell at a
    /// prompt either way, with the line editor's buffer cleared (a ^C that was NOT
    /// a signal is a byte zle may still hold).
    fn ctrl_c_interrupts_sleep(&mut self) -> bool {
        self.ctrl_c_status("") == "130"
    }

    /// Type `<runner>sh -c 'echo SLEEP42; exec sleep 4'`, send Ctrl-C once the
    /// command runs, and return its status. "Runs" is the command's OWN word: a
    /// foreground job owns the tty before it execs (both shells' children call
    /// `tcsetpgrp` first), so a ^C after `SLEEP42` reaches `sh` or the `sleep` it
    /// became, never the shell. A fixed pause after `133;C` stood in for that and
    /// under load sent the ^C while zsh was still forking, where it interrupted
    /// nothing (measured 2026-09-29 in the merge gate: `133;D;0` after `^C`).
    fn ctrl_c_status(&mut self, runner: &str) -> String {
        let from = self.len();
        self.send_bytes(format!("{runner}sh -c 'echo SLEEP$((40+2)); exec sleep 4'\r").as_bytes());
        let running = self.wait_for_after("SLEEP42", from);
        self.send_bytes(b"\x03");
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
        let from = self.len();
        self.send_bytes(b"read -r x; echo \"GOT=[$x]\"\r");
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
        let from = self.len();
        self.send_bytes(format!("{}\r", end.line()).as_bytes());
        let raw = self.wait_for_after("RAW42", from);
        let done = self.wait_for_after(&format!("\x1b]133;D;{}", end.status()), raw);
        self.wait_for_after("\x1b]133;A", done);
    }

    /// The shell's own report of its tty freeze (zsh `ttyctl`).
    fn zsh_frozen(&mut self) -> bool {
        let from = self.len();
        self.send_bytes(b"ttyctl; echo TTYCTL$((40+2))\r");
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
/// `sleep 4` and a CR ends `read`.
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
        sh.ctrl_c_interrupts_sleep(),
        "{}: {what}: Ctrl-C did not end `sleep 4` (its status was not 130)\n{}",
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
        !sh.ctrl_c_interrupts_sleep(),
        "{}: {what}: the control's Ctrl-C ended `sleep 4` — the measurement cannot tell\n{}",
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
/// tab, in the fixture's home, with `extra` in its environment.
#[cfg(unix)]
fn spawn_pty_shell(
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
    let mut cmd = Command::new("script");
    if cfg!(target_os = "macos") {
        cmd.args(["-q", "/dev/null"]).args(&argv);
    } else {
        let quoted: Vec<String> = argv
            .iter()
            .map(|a| format!("'{}'", a.replace('\'', "'\\''")))
            .collect();
        cmd.args(["-q", "-c", &quoted.join(" "), "/dev/null"]);
    }
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
        .env("TERM", "xterm-256color")
        .env_remove("BASH_ENV");
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
    let mut sh = LiveShell::spawn(label, &mut cmd).holding(hermetic.into_home());
    sh.wait_for_after("\x1b]133;A", 0);
    sh
}

/// A zsh and a pty, or a skip that says so.
#[cfg(unix)]
fn live_pty_zsh() -> Option<&'static str> {
    let Some(zsh) = zsh_shell() else {
        eprintln!("zsh not installed; skipping (ATERM_TEST_ZSH=<zsh> runs it)");
        return None;
    };
    if !script_can_allocate_a_pty() {
        eprintln!("no `script` to allocate a pty; skipping");
        return None;
    }
    Some(zsh)
}

/// A NEW integrated zsh: a raw-mode child dying by SIGKILL, `exit 1` or `exit 0`
/// leaves the next command's tty sane — Ctrl-C ends `sleep 4`, a CR ends `read`
/// — and the user's own `stty -ixon`, typed before, survives all three (the
/// freeze puts back exactly what the shell had). The shell reports its tty
/// frozen. The same scenario from the unfixed scripts is the negative control.
#[cfg(unix)]
#[test]
fn an_integrated_zsh_keeps_its_tty_when_a_raw_program_dies() {
    let Some(zsh) = live_pty_zsh() else { return };
    let fx = LiveFixture::new();
    std::fs::write(fx.home.join(".zshrc"), "").expect(".zshrc");
    let ours = install_script_set(&fx.base).expect("install");

    // The control: the unfixed scripts reproduce the owner's tab.
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

/// A user whose rc defines `stty` as a FUNCTION (or `reset` as an ALIAS) keeps
/// zsh's adopting behaviour (a frozen tty would silently undo what their
/// definition does): the tty is not frozen, and the repair still runs after a
/// FAILED command — a raw child's SIGKILL and `exit 1` leave the next command
/// sane, `stty -ixon` included.
#[cfg(unix)]
#[test]
fn an_integrated_zsh_with_its_own_stty_is_repaired_not_frozen() {
    let Some(zsh) = live_pty_zsh() else { return };
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

    // An ALIAS for one of the three names rules it out the same way.
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

/// A zsh that was ALREADY RUNNING — and already broken — when the fix shipped
/// gets it through the loader channel: started from the unfixed scripts, a
/// SIGKILLed raw child breaks it (the in-test negative control); the host then
/// points it at this build's folder, and at its next prompt the new body both
/// REPAIRS the tty it had adopted and freezes it — so the next command, and the
/// next raw child's death, leave Ctrl-C and Enter working.
#[cfg(unix)]
#[test]
fn a_running_zsh_with_a_broken_tty_is_repaired_by_the_body_the_loader_delivers() {
    let Some(zsh) = live_pty_zsh() else { return };
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
/// only after a SIGNAL death) the next command's tty is sane, and a SIGKILL
/// stays fine; a deliberate `stty -ixon` survives. The unfixed scripts are the
/// negative control, on the `exit 1` that breaks them.
#[cfg(unix)]
#[test]
fn an_integrated_bash_repairs_its_tty_after_a_raw_program_fails() {
    if !script_can_allocate_a_pty() {
        eprintln!("no `script` to allocate a pty; skipping");
        return;
    }
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
    // bash's own restore after a SIGNAL death puts IXON back too; the repair
    // after an `exit 1` cannot tell a raw mode's IXON-off from the user's and
    // leaves it off (which bash then keeps, as the state it saved).
    for (end, ixon) in [(RawEnd::Sigkill, true), (RawEnd::Exit1, false)] {
        sh.kill_raw_child(end);
        assert_tty_usable(&mut sh, &format!("after a raw child's {end:?}"), ixon);
    }
    sh.run_line("stty ixon", "0");
    sh.run_line("stty -ixon", "0");
    sh.kill_raw_child(RawEnd::Exit1);
    assert_tty_usable(&mut sh, "stty -ixon, then a raw child's exit 1", false);
    sh.finish();
}

/// A user whose rc sets `setopt err_return`: the
/// thawing wrapper ran under the USER's options, so a failing wrapped command —
/// `stty bogusflag`, a typo — returned from `__aterm_tty_thaw_run` before its
/// `ttyctl -f`. The shell stayed THAWED for good while `__aterm_tty_frozen` still
/// said 1, which also switches the fallback repair off: measured in a pty, a raw
/// child's `exit 1` then left `isig=off iexten=off icrnl=off opost=off ixon=off`
/// and Ctrl-C into `sleep 3` took 3.03–4.04 s — the original defect, in full. The
/// wrapper now opens with `emulate -L zsh` and re-freezes in an `always` block.
/// The failing command's status must still reach the prompt (`133;D;1`), the
/// shell must report its tty frozen, and a raw child dying by `exit 1` AND by
/// `exit 0` (which no fallback repair catches) must leave the next command sane.
#[cfg(unix)]
#[test]
fn an_integrated_zsh_with_err_return_stays_frozen_after_a_failing_stty() {
    let Some(zsh) = live_pty_zsh() else { return };
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
/// path too. Driven through `__aterm_tty_thaw_run` with a `sleep` so the timing is
/// the test's, not a terminal database's; the shell must report its tty frozen
/// afterwards, and a raw child's `exit 0` must then leave the next command sane.
#[cfg(unix)]
#[test]
fn an_integrated_zsh_refreezes_when_a_thawed_command_is_interrupted() {
    let Some(zsh) = live_pty_zsh() else { return };
    let fx = LiveFixture::new();
    std::fs::write(fx.home.join(".zshrc"), "").expect(".zshrc");
    let ours = install_script_set(&fx.base).expect("install");
    let mut sh = spawn_pty_shell(ShellType::Zsh, zsh, &fx, &ours, &[]);
    assert!(sh.zsh_frozen(), "zsh: the integrated shell freezes its tty");
    let status = sh.ctrl_c_status("__aterm_tty_thaw_run ");
    assert_eq!(
        status,
        "130",
        "zsh: Ctrl-C ended the thawed `sleep 4`\n{}",
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

/// The folder of the body from BEFORE the lead (16 lowercase hex no build
/// writes): what a rollback, or an older build adopting the shell, points it at.
#[cfg(unix)]
const BEFORE_LEAD_REV: &str = "00000000000000bb";

/// That body's zle-line-init, as main had it until 2026-10-01, defined last in
/// this build's body: no per-prompt re-assertion, and the loader's widget marks
/// every line it is reached on.
#[cfg(unix)]
const BEFORE_LEAD_ZSH: &str = "\
__aterm_line_init_wire() { :; }
__aterm_zle_line_init() {
    __aterm_mark_command_start
    (( ${+widgets[__aterm_orig_zle_line_init]} )) && zle __aterm_orig_zle_line_init
}
";

/// Every prompt of an integrated zsh carries ONE 133;B, whatever its ~/.zshrc
/// does to zle-line-init after the loader wired it, and every widget the rc put
/// there still runs once per line — through a body switch to the build before
/// (a rollback) and back. The rc runs AFTER the loader (zsh reads it from the
/// ZDOTDIR .zshenv), so:
///
/// - one that REPLACES zle-line-init (oh-my-zsh's `echoti smkx` widget, vi-mode
///   cursor snippets) left no prompt a 133;B — and the engine, which takes C only
///   after a B, kept no block and no exit code for any command. Here it is also
///   WRAPPED again by a precmd hook at the first prompt (powerlevel10k does that
///   over oh-my-zsh), and the wrapper must still run the replacing widget, which
///   reads in `$?` the status of the command before the line, as in bare zsh
///   (behind the lead it read 0: the lead's own commands ran first);
/// - one that WRAPS it (powerlevel10k alone, add-zle-hook-widget) already chained
///   the loader's widget: the line must not be marked twice, nor loop. After a
///   switch to the build before, that build's widget marks the line once more
///   where the chain reaches it (the engine ignores a B outside a prompt) — and
///   never loops, which re-pointing the loader's alias did (measured: ~250 marks
///   and FUNCNEST errors at every prompt);
/// - one that wraps it AGAIN at every prompt, from a precmd hook, whenever its
///   wrapper no longer holds the front: a lead that took the front back at every
///   precmd was wrapped each time, and the wrapper must still run once;
/// - one that HOOKS it with add-zle-hook-widget, at load and again from a precmd
///   hook: the dispatcher stops at the first hook that fails, and the loader's
///   widget is that first hook (the build before returned 1 there, so the user's
///   hooks never ran — nor do they after a switch to it), and the line carries
///   one 133;B however often the hook is added;
/// - one that wraps it again whenever its wrapper lost the front, under a NEW
///   name each time (zsh-autosuggestions does, once `zle-*` is out of its ignore
///   list): a lead that took the front back at every precmd was wrapped again at
///   every prompt, and each line ran every wrapper so far — until FUNCNEST
///   stopped the line (measured: from prompt 139 on);
/// - one that READS the table to choose: at its first precmd, wrapping a widget
///   listed under another function's name and replacing one listed under its own
///   (zsh-vi-mode), and while it expands its first prompt, wrapping whatever
///   holds zle-line-init (powerlevel10k). A lead found in front by the first made
///   it wrap where bare zsh replaces, so the replaced widget kept running
///   (oh-my-zsh's `echoti smkx`: the cursor-key and keypad modes stayed set
///   while every command ran); the widget it replaces runs NEVER, as in bare zsh;
/// - one that registers through add-zle-hook-widget where the dispatcher holds
///   the front and wraps otherwise (powerlevel10k, again while it expands its
///   first prompt), over a dispatcher, with a hook added from a later precmd: a
///   lead in front of the dispatcher made it wrap, and that later hook made the
///   dispatcher call the wrapper that calls the dispatcher;
/// - one whose zle-line-init returns non-zero, which a user's ZERR trap sees
///   once, from that widget, as in bare zsh;
/// - one whose precmd hook fails with an error, after which zsh runs no due
///   `sched` event until zle reads the line.
///
/// Every rc also sets PRINT_EXIT_VALUE, and the lines' statuses cycle through 3, 1
/// and 0: zsh reports a failed command's status once, below the command. The
/// event that hands a status of 2 or more back reported it again, above the next
/// prompt. And every rc sets WARN_NESTED_VAR and WARN_CREATE_GLOBAL, which a
/// function of aterm's that runs with the user's options (the precmd, the
/// preexec, the widgets) must give nothing to warn about, and a ZERR trap, which
/// fires for the failed command as in bare zsh and never inside aterm's precmd.
/// And every rc sets VERBOSE, which echoes the text of a scheduled event before
/// it runs, and every line of a file sourced — the body, at a switch: aterm's
/// own code (`__aterm…`) never reaches the user's screen, and each typed line is
/// still echoed once, as in bare zsh.
///
/// After a switch to the build before, a line is marked where that build's
/// widget is reached, and no more often than twice. The build before is also the
/// negative control: from its folder, the replacing rc's prompts carry no
/// 133;B.
#[cfg(unix)]
#[test]
fn an_integrated_zsh_marks_every_prompt_whatever_its_rc_does_to_zle_line_init() {
    let Some(zsh) = live_pty_zsh() else { return };
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
    // status (`zsh: exit <status>`, PRINT_EXIT_VALUE). Each line is typed once
    // zle owns the tty, so its only echo is VERBOSE's: typed at the bare 133;A,
    // a loaded machine let the tty echo it ahead of the prompt too (2026-10-05
    // merge gate: `\n(exit 3)\r\n` twice in one prompt).
    let lines = [("(exit 3)", "3"), ("false", "1"), ("true", "0")];
    let prompts = |sh: &mut LiveShell, at: &mut usize, n: usize| -> Vec<(String, usize, &str)> {
        (0..n)
            .map(|i| {
                let (line, status) = lines[i % lines.len()];
                sh.wait_for_zle(*at);
                let next = sh.run_line(line, status);
                let prompt = stdout_between(sh, *at, next);
                *at = next;
                (prompt, usize::from(status != "0"), line)
            })
            .collect()
    };

    // Every rc also reports the status of a command that fails, as zsh's own
    // PRINT_EXIT_VALUE does: once, below that command; warns at a function that
    // sets a global by a plain assignment; echoes what it runs (VERBOSE); and
    // traps ZERR, naming the function the failed command ran in.
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

/// A user's XTRACE traces the user's commands, not aterm's, and never the
/// capability nonce. Bare zsh traces nothing between two commands; aterm's load,
/// precmd, preexec and scheduled event traced every line they ran — 80 to 660
/// per command — and the nonce in them at every mark, into the screen and the
/// scrollback. XTRACE comes from `.zshenv` here, so the load runs under it too,
/// and the rc replaces zle-line-init (oh-my-zsh's shape), so the event runs: its
/// own line, which zsh prints before it runs, is all of aterm's that is left —
/// `stty`'s wrapper included, as bare zsh traces only the line that ran it.
#[cfg(unix)]
#[test]
fn an_integrated_zsh_under_xtrace_traces_the_users_commands_and_not_aterms() {
    let Some(zsh) = live_pty_zsh() else { return };
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
    // (`+zsh:<n>> …`); a line traced INSIDE a function of aterm's carries its
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

/// bash's twin of the test above: a user's XTRACE traces the user's commands
/// and their PROMPT_COMMAND, not aterm's, and never the capability nonce. Bare
/// bash traces one line per command here; aterm's DEBUG-trap preexec and
/// PROMPT_COMMAND traced about 165, the nonce about 18 times among them (bash
/// 3.2.57 and 5.2.37). XTRACE comes from `.bashrc`, so the load runs under it
/// too, and PS4 names the function a line runs in: what is left of aterm's is
/// bash's own trace of the two hooks' calls, at the top level, and the user's
/// PROMPT_COMMAND, which aterm's prompt handler runs.
#[cfg(unix)]
#[test]
fn an_integrated_bash_under_xtrace_traces_the_users_commands_and_not_aterms() {
    if !script_can_allocate_a_pty() {
        eprintln!("no `script` to allocate a pty; skipping");
        return;
    }
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
    // (the function a traced line runs in, the line)
    let traced: Vec<(&str, &str)> = text
        .lines()
        .filter_map(|line| line.strip_prefix('+'))
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
            !line.contains("__aterm")
                || matches!(
                    line.split_whitespace().next(),
                    Some("__aterm_preexec" | "__aterm_prompt_command")
                )
        };
        assert!(
            users,
            "bash: aterm's code in the trace: +{frame}> {line}\n{text}"
        );
    }
    sh.finish();
}

/// A user's ERR trap, `set -e` and `set -o functrace` meet the commands bare bash
/// shows them. aterm's PROMPT_COMMAND returned the command's status and ran a
/// `( exit <status> )` before the user's own PROMPT_COMMAND — failed commands
/// both — so the ERR trap fired again at the prompt after every failure, and
/// `set -e` closed the shell at the prompt after a failure bash itself lets pass
/// (`false && true`; on bash 5 so did a missing atpkg hook's failed open). In a
/// tab with atpkg's agents/ it still did, at the prompt after any command that
/// hashed a name (`hash -d claude codex` fails for a name not hashed) and after
/// atpkg wrote its hook (the reroute derivation says no). Under functrace the
/// DEBUG trap fired inside aterm's own prompt functions: every prompt reported
/// one of their lines as the user's command, and the command typed at it got no
/// 633;E — and without functrace a function wrapping the trap (starship's
/// layering on bash < 4.4) is no function the trap fired in: the commands typed
/// after it are reported. The user's PROMPT_COMMAND still reads the command's
/// `$?`.
#[cfg(unix)]
#[test]
fn an_integrated_bash_keeps_the_users_err_trap_errexit_and_functrace_as_bare_bash_does() {
    if !script_can_allocate_a_pty() {
        eprintln!("no `script` to allocate a pty; skipping");
        return;
    }
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
    sh.send_bytes(b"\r");
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
        ["f", "echo\\x20hi"],
        "bash: the commands 633;E reported under functrace\n{}",
        stdout_between(&sh, from, sh.len())
    );
    sh.run_line("set +o functrace", "0");
    // starship's layering when it finds a DEBUG trap (bash < 4.4), typed.
    let from = sh.run_line(
        r#"w() { local a=$1; eval -- "$T"; : "$a"; }; eval "T=($(trap -p DEBUG))"; T=("${T[2]}"); trap 'w "$_"' DEBUG"#,
        "0",
    );
    sh.send_bytes(b"echo wrapped$((40+2))\r");
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

/// The user's `$_` reaches the user's code as in bare bash: `mkdir -p d && cd
/// "$_"` enters d, a command reads the last argument of the one before it, and so
/// does the user's PROMPT_COMMAND. The DEBUG trap's own command is the last one
/// bash runs before each of the user's, and called bare it left `$_` reading
/// `__aterm_preexec` (`cd: __aterm_preexec: No such file or directory`); the
/// user's PROMPT_COMMAND read the last argument of a command of aterm's.
#[cfg(unix)]
#[test]
fn an_integrated_bash_keeps_the_users_last_argument() {
    if !script_can_allocate_a_pty() {
        eprintln!("no `script` to allocate a pty; skipping");
        return;
    }
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
