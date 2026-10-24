// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#[cfg(unix)]
const UNFIXED_REV: &str = "00000000000000aa";

#[cfg(unix)]
const ZSH_TTY_CALLS: [&str; 2] = [
    "    (( __aterm_tty_armed )) || __aterm_tty_arm\n",
    "    (( __aterm_tty_frozen || last_status == 0 )) || __aterm_tty_repair\n",
];
#[cfg(unix)]
const BASH_TTY_CALLS: [&str; 1] =
    ["    [[ \"$last_status\" == 0 ]] && ! __aterm_line_ran_a_second || __aterm_tty_repair\n"];

#[cfg(unix)]
#[derive(Clone, Copy, Debug)]
enum RawEnd {
    Sigkill,
    Exit1,
    Exit0,
    Exit0Late,
}

#[cfg(unix)]
impl RawEnd {
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

#[cfg(unix)]
const FLAGS_LINE: &str = "__s=\" $(command stty -a | tr '\\n' ' ') \"; for __f in isig icanon iexten icrnl opost ixon; do case \"$__s\" in *\" -$__f \"*) printf '%s=off ' \"$__f\";; *) printf '%s=on ' \"$__f\";; esac; done; echo FLAGS$((40+2))";

#[cfg(unix)]
impl LiveShell {
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

    fn run_line(&mut self, line: &str, status: &str) -> usize {
        let from = self.type_at_prompt(format!("{line}\r").as_bytes());
        let done = self.wait_for_after(&format!("\x1b]133;D;{status}"), from);
        self.wait_for_after("\x1b]133;A", done)
    }

    fn type_at_prompt(&mut self, bytes: &[u8]) -> usize {
        self.wait_for_line_editor();
        let from = self.len();
        self.send_bytes(bytes);
        from
    }

    fn wait_for_line_editor(&mut self) -> usize {
        const OWNS_THE_TTY: [&[u8]; 2] = [b"\x1b[?2004h", b"\x1b]133;B"];
        let prompt = self.last_prompt();
        let pipe = std::sync::Arc::clone(&self.stdout);
        self.wait_on(&pipe, "the line editor's \\e[?2004h or 133;B", |out| {
            let after = &out[prompt.min(out.len())..];
            OWNS_THE_TTY
                .iter()
                .filter_map(|word| find_bytes(after, word).map(|at| prompt + at + word.len()))
                .min()
        })
    }

    fn last_prompt(&self) -> usize {
        const PROMPT: &[u8] = b"\x1b]133;A";
        let out = self
            .stdout
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        out.windows(PROMPT.len())
            .rposition(|w| w == PROMPT)
            .map_or(0, |at| at + PROMPT.len())
    }

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

    fn ctrl_c_interrupts(&mut self) -> bool {
        self.ctrl_c_status("") == "130"
    }

    fn ctrl_c_status(&mut self, runner: &str) -> String {
        let from = self.type_at_prompt(
            format!("{runner}sh -c 'echo WAIT$((40+2)); exec head -c 1 >/dev/null'\r").as_bytes(),
        );
        let running = self.wait_for_after("WAIT42", from);
        self.send_bytes(b"\x03\n");
        let status = self.status_after(running);
        self.send_bytes(b"\x15");
        self.settle();
        status
    }

    fn status_after(&mut self, from: usize) -> String {
        let done = self.wait_for_after("\x1b]133;D;", from);
        self.wait_for_after("\x1b]133;A", done);
        stdout_between(self, done, (done + 3).min(self.len()))
    }

    fn cr_ends_read(&mut self) -> bool {
        let from = self.type_at_prompt(b"read -r x; echo \"GOT=[$x]\"\r");
        let started = self.wait_for_after("\x1b]133;C", from);
        self.send_bytes(b"hello\r\n");
        let got = self.wait_for_after("GOT=[", started);
        let end = self.wait_for_after("]", got);
        let done = self.wait_for_after("\x1b]133;D;", end);
        self.wait_for_after("\x1b]133;A", done);
        self.settle();
        stdout_between(self, got, end - 1) == "hello"
    }

    fn settle(&mut self) {
        let from = self.type_at_prompt(b"echo SETTLED$((40+2))\r");
        let settled = self.wait_for_after("SETTLED42", from);
        self.wait_for_after("\x1b]133;A", settled);
    }

    fn kill_raw_child(&mut self, end: RawEnd) {
        let from = self.type_at_prompt(format!("{}\r", end.line()).as_bytes());
        let raw = self.wait_for_after("RAW42", from);
        let done = self.wait_for_after(&format!("\x1b]133;D;{}", end.status()), raw);
        self.wait_for_after("\x1b]133;A", done);
    }

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

#[cfg(unix)]
#[test]
fn the_tty_probes_leave_the_shell_at_its_last_prompt() {
    let zsh = need_zsh();
    let fx = LiveFixture::new();
    std::fs::write(fx.home.join(".zshrc"), "").expect(".zshrc");
    std::fs::write(fx.home.join(".bashrc"), "").expect(".bashrc");
    let ours = install_script_set(&fx.base).expect("install");
    type Probe = fn(&mut LiveShell) -> bool;
    let probes: [(&str, Probe); 2] = [
        ("Ctrl-C into `head -c 1`", LiveShell::ctrl_c_interrupts),
        ("a CR into `read`", LiveShell::cr_ends_read),
    ];
    let mut shells = ShellLanes::default();
    let mut lanes: Vec<_> = shells
        .bashes()
        .into_iter()
        .map(|bash| (ShellType::Bash, bash))
        .collect();
    lanes.push((ShellType::Zsh, zsh));
    for (shell, program) in lanes {
        let mut sh = spawn_pty_shell(shell, program, &fx, &ours, &[]);
        for (probe, ends) in probes {
            assert!(
                ends(&mut sh),
                "{}: {probe} on a sane tty\n{}",
                sh.label,
                sh.transcript()
            );
            let anchor = sh.last_prompt();
            let from = sh.type_at_prompt(b"echo NEXT$((40+2))\r");
            let next = sh.wait_for_after("NEXT42", from);
            let ran = stdout_between(&sh, anchor, next);
            let started = ran.rfind("\x1b]133;C").unwrap_or_else(|| {
                panic!("{}: no 133;C before NEXT42\n{}", sh.label, sh.transcript())
            });
            assert!(
                !ran[..started].contains("\x1b]133;A"),
                "{}: after {probe}, the shell drew another prompt after the one the next line \
                 was typed at\n{ran:?}",
                sh.label
            );
            sh.wait_for_after("\x1b]133;A", next);
        }
        sh.finish();
    }
}

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

#[cfg(unix)]
type ScriptEdits<'a> = (&'a str, &'a str, &'a [(&'a str, &'a str)]);

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

#[cfg(unix)]
fn spawn_pty_shell(
    shell: ShellType,
    program: &str,
    fx: &LiveFixture,
    dir: &Path,
    extra: &[(&str, &str)],
) -> LiveShell {
    let mut sh = spawn_pty_shell_unwaited(&[], shell, program, fx, dir, extra);
    sh.wait_for_after("\x1b]133;A", 0);
    sh
}

#[cfg(unix)]
fn spawn_pty_shell_unwaited(
    under: &[&str],
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
    let mut argv: Vec<String> = under.iter().map(|word| (*word).to_owned()).collect();
    argv.push(program.to_owned());
    if let Some(over) = argv_override {
        argv.extend(over.into_iter().skip(1));
    }
    argv.push("-i".to_owned());
    let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
    let mut cmd = pty_command(&argv);
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
        _ => program,
    };
    LiveShell::spawn_on_pty(label, &mut cmd).holding(hermetic.into_home())
}

#[cfg(unix)]
fn live_pty_zsh() -> &'static str {
    need_zsh()
}

#[cfg(unix)]
#[test]
fn an_integrated_zsh_keeps_its_tty_when_a_raw_program_dies() {
    let zsh = live_pty_zsh();
    let fx = LiveFixture::new();
    std::fs::write(fx.home.join(".zshrc"), "").expect(".zshrc");
    let ours = install_script_set(&fx.base).expect("install");

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
    sh.run_line("own() { local options=mine; stty -ixon; }; own", "0");
    sh.kill_raw_child(RawEnd::Sigkill);
    assert_tty_usable(&mut sh, "stty -ixon, then a SIGKILLed raw child", false);
    sh.run_line("stty ixon", "0");
    sh.kill_raw_child(RawEnd::Exit1);
    assert_tty_usable(&mut sh, "stty ixon again, then a raw child's exit 1", true);
    sh.finish();
}

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

#[cfg(unix)]
const ZSH_FIRST_INTR_CALL: &str =
    "        zle -F $__aterm_tty_probe_fd __aterm_tty_probe_ready 2>/dev/null\n";

#[cfg(unix)]
const ZLE_INTR_WAKE: &[u8] = b" ";

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
            sh.wait_for_after("@@P@@", at);
        }
        let typed = sh.len();
        sh.send_bytes(b"echo LO$((7*7))");
        let from = sh.wait_for_after("))", typed);
        sh.send_bytes(b"\x03");
        sh.send_bytes(ZLE_INTR_WAKE);
        if fixed {
            sh.wait_for_after("\x1b]133;A", from);
            sh.wait_for_line_editor();
        }
        let from = sh.len();
        sh.send_bytes(b"echo KEPT$((40+2)) $?\r");
        let kept = sh.wait_for_after("KEPT42", from);
        let ran = cut_osc(&stdout_between(&sh, from, kept));
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

    std::fs::write(fx.home.join(".zshrc"), "alias reset='command reset'\n").expect(".zshrc");
    let mut sh = spawn_pty_shell(ShellType::Zsh, zsh, &fx, &ours, &[]);
    assert!(
        !sh.zsh_frozen(),
        "zsh: a user's own `reset` alias rules the freeze out"
    );
    sh.kill_raw_child(RawEnd::Sigkill);
    assert_tty_usable(&mut sh, "own reset alias, a SIGKILLed raw child", false);
    sh.finish();
}

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

#[cfg(unix)]
#[test]
fn an_integrated_bash_repairs_its_tty_after_a_raw_program_fails() {
    let mut shells = ShellLanes::default();
    for bash in shells.bashes() {
        let fx = LiveFixture::new();
        std::fs::write(fx.home.join(".bashrc"), "").expect(".bashrc");
        let ours = install_script_set(&fx.base).expect("install");

        let unfixed = unfixed_folder(&fx.base);
        let mut sh = spawn_pty_shell(ShellType::Bash, bash, &fx, &unfixed, &[]);
        sh.kill_raw_child(RawEnd::Exit1);
        assert_tty_broken(&mut sh, "unfixed, after a raw child's exit 1");
        sh.finish();

        let mut sh = spawn_pty_shell(ShellType::Bash, bash, &fx, &ours, &[]);
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
}

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

#[cfg(unix)]
const BEFORE_LEAD_REV: &str = "00000000000000bb";

#[cfg(unix)]
const BEFORE_LEAD_ZSH: &str = "\
__aterm_line_init_wire() { :; }
__aterm_zle_line_init() {
    __aterm_mark_command_start
    (( ${+widgets[__aterm_orig_zle_line_init]} )) && zle __aterm_orig_zle_line_init
}
";

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
    for line in text.lines().filter(|line| line.contains("__aterm")) {
        assert!(
            line.contains("__aterm_line_init_late") && !line.contains("+__aterm"),
            "zsh: aterm's code in the trace: {line}\n{text}"
        );
    }
    sh.finish();
}

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

#[cfg(unix)]
#[test]
fn an_integrated_bash_under_xtrace_traces_the_users_commands_and_not_aterms() {
    let mut shells = ShellLanes::default();
    for bash in shells.bashes() {
        let fx = LiveFixture::new();
        let dir = install_script_set(&fx.base).expect("install");
        std::fs::write(
            fx.home.join(".bashrc"),
            "PS4='+${FUNCNAME[0]-}> '\nset -x\nPROMPT_COMMAND='echo pc$((40+2))'\n",
        )
        .expect(".bashrc");
        let mut sh = spawn_pty_shell(
            ShellType::Bash,
            bash,
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
            "{bash}: the capability nonce reached the trace\n{text}"
        );
        let traced: Vec<(&str, &str)> = text
            .lines()
            .filter_map(|line| line.rsplit('\r').next()?.strip_prefix('+'))
            .filter_map(|line| line.trim_start_matches('+').split_once("> "))
            .map(|(frame, line)| (frame, line.trim_end()))
            .collect();
        for line in ["echo traced42", "cd /", "echo pc42"] {
            assert!(
                traced.iter().any(|(_, traced)| *traced == line),
                "{bash}: the user's `{line}` is traced\n{text}"
            );
        }
        for (frame, line) in &traced {
            let users = if frame.starts_with("__aterm") {
                matches!(*line, "eval 'echo pc$((40+2))'" | "echo pc42")
            } else {
                !line.contains("__aterm")
                    || matches!(
                        line.split_whitespace().next(),
                        Some("__aterm_preexec" | "__aterm_prompt_command" | "__aterm_ps0")
                    )
            };
            assert!(
                users,
                "{bash}: aterm's code in the trace: +{frame}> {line}\n{text}"
            );
        }
        sh.finish();
    }
}

#[cfg(unix)]
#[test]
fn an_integrated_bash_keeps_the_users_err_trap_errexit_and_functrace_as_bare_bash_does() {
    let mut shells = ShellLanes::default();
    for bash in shells.bashes() {
        let fx = LiveFixture::new();
        let dir = install_script_set(&fx.base).expect("install");
        fx.lay_dirs();
        std::fs::write(
            fx.home.join(".bashrc"),
            "trap 'echo err$((40+2))' ERR\nPROMPT_COMMAND='echo \"pc=$?\"'\n",
        )
        .expect(".bashrc");
        let mut sh = spawn_pty_shell(
            ShellType::Bash,
            bash,
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
            "{bash}: the ERR trap fires once for one `false`\n{}",
            sh.transcript()
        );
        assert!(
            text.contains("pc=1"),
            "{bash}: the user's PROMPT_COMMAND reads the command's `$?`\n{}",
            sh.transcript()
        );
        sh.run_line("set -e", "0");
        sh.run_line("false && true", "1");
        sh.run_line("ls -d / >/dev/null", "0");
        fx.lay_hook("bash", ATPKG_HOOK_POSIX_GOLDEN);
        sh.run_line("echo alive$((40+2))", "0");
        let from = sh.run_line("set -o functrace", "0");
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
            "{bash}: the commands 633;E reported under functrace\n{}",
            stdout_between(&sh, from, sh.len())
        );
        sh.run_line("set +o functrace", "0");
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
            "{bash}: the command 633;E reported behind a function wrapping the trap\n{}",
            stdout_between(&sh, from, end)
        );
        sh.finish();
    }
}

#[cfg(unix)]
#[test]
fn an_integrated_bash_chains_the_debug_trap_its_rc_set() {
    let mut shells = ShellLanes::default();
    for bash in shells.bashes() {
        let fx = LiveFixture::new();
        std::fs::write(
            fx.home.join(".bashrc"),
            "HISTCONTROL=ignorespace\n\
             trap '[[ $BASH_COMMAND != \"echo typed\"* ]] ||\n  echo \"DBG'\\''$((40+2))\"' DEBUG\n",
        )
        .expect(".bashrc");
        let ours = install_script_set(&fx.base).expect("install");
        let unhanded = folder_with_edits(&fx.base, "00000000000000ac", &[]);
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
                    "{bash} (channel {channel}): the rc's DEBUG trap ran (chained {chained}) and \
                     aterm marked the command (marked {marked})\n{}",
                    sh.transcript()
                );
                let handed = std::fs::read_to_string(&trap_file).ok();
                assert_eq!(
                    handed.as_deref(),
                    channel.then_some(
                        "trap -- '[[ $BASH_COMMAND != \"echo typed\"* ]] ||\n  echo \"DBG'\\''$((40+2))\"' DEBUG\n"
                    ),
                    "{bash} (channel {channel}): the wrapper hands the trap over in the session \
                     file exactly when there is one"
                );
            } else {
                assert!(
                    !(chained && marked),
                    "{bash}: the control must lose the rc's trap or aterm's mark\n{}",
                    sh.transcript()
                );
            }
            sh.finish();
        }
    }
}

#[cfg(unix)]
const BASH_HANDOVER_GUARD: &str = "if [ -z \"${ATERM_SHELL_INTEGRATION_INSTALLED:-}\" ]; then\n";

#[cfg(unix)]
#[test]
fn an_integrated_bash_whose_rc_sources_the_integration_keeps_its_marks() {
    let mut shells = ShellLanes::default();
    for bash in shells.bashes() {
        let fx = LiveFixture::new();
        std::fs::write(
            fx.home.join(".bashrc"),
            "HISTCONTROL=ignorespace\n\
             . \"$ATERM_SHELL_INTEGRATION_DIR/aterm_shell_integration.bash\"\n",
        )
        .expect(".bashrc");
        let ours = install_script_set(&fx.base).expect("install");
        let unguarded = folder_with_edits(&fx.base, "00000000000000ad", &[]);
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
                "{bash} (channel {channel}, guarded {fixed}): aterm marked the typed command\n{}",
                sh.transcript()
            );
            sh.finish();
        }
    }
}

#[cfg(unix)]
#[test]
fn an_integrated_bash_whose_rc_set_errexit_and_an_err_trap_loads_quietly() {
    let mut shells = ShellLanes::default();
    for bash in shells.bashes() {
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
            "{bash}: the load reported a failed command to the rc's ERR trap\n{}",
            sh.transcript()
        );
        assert!(
            stdout_between(&sh, loaded, ran).contains("DBG42"),
            "{bash}: the rc's DEBUG trap chained\n{}",
            sh.transcript()
        );
        sh.finish();
    }
}

#[cfg(unix)]
#[test]
fn an_integrated_bash_reports_the_line_as_typed() {
    let mut shells = ShellLanes::default();
    for bash in shells.bashes() {
        let fx = LiveFixture::new();
        let dir = install_script_set(&fx.base).expect("install");
        std::fs::write(
            fx.home.join(".bashrc"),
            "alias ll='ls -alF'\nHISTCONTROL=ignorespace\n\
             PROMPT_COMMAND='history -a; history -c; history -r'\n",
        )
        .expect(".bashrc");
        let mut sh = spawn_pty_shell(ShellType::Bash, bash, &fx, &dir, &[("ATERM_CHILD", "1")]);
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
            "{bash}: the lines 633;E reported\n{}",
            sh.transcript()
        );
        assert!(
            text.contains("\x1b]0;true && echo a$((40+2))\x07"),
            "{bash}: the title while the line runs is the line\n{}",
            sh.transcript()
        );
        sh.finish();
    }
}

#[cfg(target_os = "macos")]
const NO_TEMP_FOLDER: &str = "(version 1)(allow default)(deny file-write* \
     (subpath \"/private/tmp\") (subpath \"/private/var/tmp\") (subpath \"/private/var/folders\"))";

/// Runs on every bash ([`ShellLanes::bashes`]): macOS's 3.2 forks every read and has no PS0
/// hook, so it checks only that the blocks need no temp folder; the `${ ...; }` read and
/// PS0's file-free hook, which bash 5.3 keeps in a file, are judged by the pinned 5.3.
#[cfg(target_os = "macos")]
#[test]
fn an_integrated_bash_with_no_writable_temp_folder_runs_every_line_with_its_block() {
    let sandbox = "/usr/bin/sandbox-exec";
    let taken = aterm_uds::spawnfd::command(sandbox)
        .args(["-p", NO_TEMP_FOLDER, "/bin/sh", "-c"])
        .arg("test ! -w /tmp && test ! -w /var/tmp")
        .output();
    if !taken.as_ref().is_ok_and(|out| out.status.success()) {
        aterm_spec::host::could_not_run(format!(
            "bash with no writable temp folder: {sandbox} takes no folder away here (inside \
             another sandbox?): {taken:?}"
        ));
    }
    let mut shells = ShellLanes::default();
    for bash in shells.bashes() {
        for (start, rc) in [
            ("naming Linux", "OSTYPE=linux-gnu\n"),
            (
                "in place over v0.108.0's PS0",
                ". \"$ATERM_SHELL_INTEGRATION_DIR/aterm_shell_integration.bash\"\n\
                 if (( BASH_VERSINFO[0] * 100 + BASH_VERSINFO[1] >= 503 )); then \
                 PS0='${ __aterm_ps0; }'; fi\n",
            ),
        ] {
            let fx = LiveFixture::new();
            let dir = install_script_set(&fx.base).expect("install");
            std::fs::write(fx.home.join(".bashrc"), format!("unset TMPDIR\ncd /\n{rc}"))
                .expect(".bashrc");
            let mut sh = spawn_pty_shell_unwaited(
                &[sandbox, "-p", NO_TEMP_FOLDER],
                ShellType::Bash,
                bash,
                &fx,
                &dir,
                &[("ATERM_SHELL_NONCE", REKEY_OLD), ("ATERM_CHILD", "1")],
            );
            sh.wait_for_after("\x1b]133;A", 0);
            for (line, status) in [
                ("echo a$((40+2))", "0"),
                ("false", "1"),
                ("true | cat", "0"),
            ] {
                let from = sh.type_at_prompt(format!("{line}\r").as_bytes());
                let end = sh.wait_for_after("\x1b]133;A", from);
                let text = stdout_between(&sh, from, end);
                let marks = [
                    format!(
                        "\x1b]633;E;{};id={REKEY_OLD}\x07",
                        line.replace(' ', "\\x20")
                    ),
                    format!("\x1b]133;C;id={REKEY_OLD}\x07"),
                    format!("\x1b]133;D;{status};id={REKEY_OLD}\x07"),
                ];
                assert!(
                    !text.contains("function_substitute") && marks.iter().all(|m| text.contains(m)),
                    "{bash} ({start}): `{line}` ran without its block {marks:?}\n{}",
                    sh.transcript()
                );
            }
            sh.finish();
        }
    }
}

#[cfg(unix)]
#[test]
fn an_integrated_bash_with_an_array_prompt_command_reports_only_typed_lines() {
    let mut shells = ShellLanes::default();
    for bash in shells.bashes() {
        let fx = LiveFixture::new();
        let dir = install_script_set(&fx.base).expect("install");
        let history = fx.home.join(".bash_history");
        std::fs::write(
            fx.home.join(".bashrc"),
            "HISTFILE=~/.bash_history\nHISTCONTROL=ignoreboth\n\
             PROMPT_COMMAND=('__x=1; __y=2' 'history -a; history -c; history -r')\n",
        )
        .expect(".bashrc");
        let mut sh = spawn_pty_shell(ShellType::Bash, bash, &fx, &dir, &[("ATERM_CHILD", "1")]);
        let reported = |text: &str| -> Vec<String> {
            text.split("\x1b]633;E;")
                .skip(1)
                .map(|rest| rest.split([';', '\x07']).next().unwrap_or("").to_owned())
                .collect()
        };
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
            "{bash}: the lines 633;E reported\n{}",
            sh.transcript()
        );
        assert!(
            !text.contains("OTHER-TAB-LINE"),
            "{bash}: another tab's line reported or titled\n{}",
            sh.transcript()
        );
        sh.finish();
    }
}

#[cfg(unix)]
#[test]
fn an_integrated_bash_keeps_the_users_last_argument() {
    let mut shells = ShellLanes::default();
    for bash in shells.bashes() {
        let fx = LiveFixture::new();
        let dir = install_script_set(&fx.base).expect("install");
        std::fs::write(
            fx.home.join(".bashrc"),
            "PROMPT_COMMAND='echo \"pc_=[$_]\"'\n",
        )
        .expect(".bashrc");
        let mut sh = spawn_pty_shell(ShellType::Bash, bash, &fx, &dir, &[("ATERM_CHILD", "1")]);
        sh.run_line(
            "cd \"$HOME\" && mkdir -p last42 && cd \"$_\"; echo \"in=${PWD##*/}\"",
            "0",
        );
        sh.run_line("echo one last$((40+1))", "0");
        let end = sh.run_line("echo \"was=[$_]\"", "0");
        let text = stdout_between(&sh, 0, end);
        for said in ["in=last42", "pc_=[last41]", "was=[last41]"] {
            assert!(text.contains(said), "{bash}: {said}\n{}", sh.transcript());
        }
        sh.finish();
    }
}

#[cfg(unix)]
#[test]
fn an_integrated_bash_keeps_its_marks_out_of_a_redirected_compound_command() {
    let mut shells = ShellLanes::default();
    for bash in shells.bashes() {
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
            let text = std::fs::read(&path)
                .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
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
        let mut sh = spawn_pty_shell(ShellType::Bash, bash, &fx, &unfixed, &nonce);
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
        let mut sh = spawn_pty_shell(ShellType::Bash, bash, &fx, &ours, &nonce);
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
                        "{bash}: `{line}`: {mark:?} did not reach the terminal after the marks before it\n{}",
                        sh.transcript()
                    );
                };
                at += found + mark.len();
            }
        };
        for (line, file, want) in compounds {
            let from = sh.len();
            let end = sh.run_line(&format!(" {line}"), "0");
            assert_eq!(read(file), want, "{bash}: ` {line}` wrote into {file}");
            assert!(
                stdout_between(&sh, from, end).contains(&format!("\x1b]133;C;id={REKEY_OLD}\x07")),
                "{bash}: ` {line}`: its 133;C did not reach the terminal\n{}",
                sh.transcript()
            );
            block(&mut sh, line);
            assert_eq!(read(file), want, "{bash}: `{line}` wrote into {file}");
        }
        block(&mut sh, "exec 3>&1 >out-exec");
        block(&mut sh, "echo in-log");
        block(&mut sh, "exec >&3 3>&-");
        assert_eq!(read("out-exec"), "in-log\n", "{bash}: `exec >out-exec`");
        sh.finish();
    }
}

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
    sh.send_bytes(b"\x15");
    sh.finish();
}
