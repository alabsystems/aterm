// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// ─── The tty settings a shell keeps after a raw-mode program dies (2026-09-26) ───
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
// Ctrl-C sent into `sleep 4` must end it (the integration's own `133;D;130`)
// within 2 s, and `read -r x` must end on a CR. The raw-mode program is `sh -c
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

    /// Like [`LiveShell::wait_for_after`], but a marker that does not appear
    /// within `within` is an answer (`None`), not a failure.
    fn wait_within(
        &mut self,
        marker: &str,
        from: usize,
        within: std::time::Duration,
    ) -> Option<usize> {
        // wasm-clock-guard: allow — this file is `include!`d only inside lib.rs's
        // `#[cfg(test)] mod tests`, so it never reaches a wasm bundle.
        let started = std::time::Instant::now();
        loop {
            {
                let out = self
                    .stdout
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if let Some(pos) = find_bytes(&out[from.min(out.len())..], marker.as_bytes()) {
                    return Some(from + pos + marker.len());
                }
            }
            if started.elapsed() >= within {
                return None;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
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

    /// Ctrl-C sent 0.3 s into `sleep 4`: did it end the sleep (the integration's
    /// `133;D;130`) within 2 s? Leaves the shell at a prompt either way, with the
    /// line editor's buffer cleared (a ^C that was NOT a signal is a byte zle may
    /// still hold).
    fn ctrl_c_interrupts_sleep(&mut self) -> bool {
        let from = self.len();
        self.send_bytes(b"sleep 4\r");
        let started = self.wait_for_after("\x1b]133;C", from);
        std::thread::sleep(std::time::Duration::from_millis(300));
        self.send_bytes(b"\x03");
        let interrupted = self
            .wait_within("\x1b]133;D;130", started, std::time::Duration::from_secs(2))
            .is_some();
        let done = self.wait_for_after("\x1b]133;D;", started);
        self.wait_for_after("\x1b]133;A", done);
        // Ctrl-U: kill-whole-line in both line editors' default emacs bindings.
        self.send_bytes(b"\x15");
        interrupted
    }

    /// `read -r x` fed `hello` + CR: did the read end, with `hello` (no CR)?
    /// When it did not, a Ctrl-J (LF) ends it so the shell comes back.
    fn cr_ends_read(&mut self) -> bool {
        let from = self.len();
        self.send_bytes(b"read -r x; echo \"GOT=[$x]\"\r");
        let started = self.wait_for_after("\x1b]133;C", from);
        std::thread::sleep(std::time::Duration::from_millis(200));
        self.send_bytes(b"hello\r");
        let ended = self
            .wait_within("GOT=[hello]", started, std::time::Duration::from_secs(2))
            .is_some();
        if !ended {
            self.send_bytes(b"\n");
        }
        let done = self.wait_for_after("\x1b]133;D;", started);
        self.wait_for_after("\x1b]133;A", done);
        ended
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
        "{}: {what}: Ctrl-C did not end `sleep 4` within 2 s\n{}",
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
    let dir = root.join(UNFIXED_REV);
    ensure_scripts(&dir).expect("unfixed scripts");
    for (ext, script, calls) in [
        ("zsh", scripts::ZSH, &ZSH_TTY_CALLS[..]),
        ("bash", scripts::BASH, &BASH_TTY_CALLS[..]),
    ] {
        let mut unfixed = script.to_owned();
        for call in calls {
            assert_eq!(
                unfixed.matches(call).count(),
                1,
                "{ext}: the fix's call site `{}` moved — keep the control in step",
                call.trim()
            );
            unfixed = unfixed.replace(call, "");
        }
        let body = scripts::body(&unfixed).expect("markers").to_owned();
        std::fs::write(dir.join(format!("aterm_shell_integration.{ext}")), &unfixed).unwrap();
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
    let mut sh = LiveShell::spawn(label, &mut cmd);
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
    // A deliberate `stty -ixon` goes through the thawing wrapper and sticks.
    sh.run_line("stty -ixon", "0");
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
    std::fs::write(&pointer, format!("{ours_rev}\n")).expect("pointer");
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

/// A user whose rc sets `setopt err_return` (review finding, 2026-09-26): the
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

/// A thawed command INTERRUPTED by Ctrl-C (review finding, 2026-09-26): zsh
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
    let from = sh.len();
    sh.send_bytes(b"__aterm_tty_thaw_run sleep 4\r");
    let started = sh.wait_for_after("\x1b]133;C", from);
    std::thread::sleep(std::time::Duration::from_millis(300));
    sh.send_bytes(b"\x03");
    assert!(
        sh.wait_within("\x1b]133;D;130", started, std::time::Duration::from_secs(2))
            .is_some(),
        "zsh: Ctrl-C ended the thawed `sleep 4`\n{}",
        sh.transcript()
    );
    let done = sh.wait_for_after("\x1b]133;D;", started);
    sh.wait_for_after("\x1b]133;A", done);
    sh.send_bytes(b"\x15");
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
