// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// ─── The warm spare's holds, on real shells (docs/DESIGN-warm-spare-2026-10-16.md) ───
//
// `include!`d into lib.rs's `#[cfg(test)] mod tests` after tests_tty.rs, whose
// helpers (`LiveShell`, `folder_with_edits`, `stdout_between`) it uses.
//
// A spare is a shell aterm spawned before anyone asked for it. Its wrapper waits
// at the PARKED hold, before any startup file, for one line on a FIFO the host
// holds open: `go` runs the startup files in view, as a cold tab would; `run`
// runs them unseen, and the integration waits again at the WARM hold, the top of
// the first prompt hook, before any prompt work. These tests drive /bin/zsh and
// /bin/bash on a real pty (`script`), started the way aterm starts a tab, with
// the FIFO at the path the shell derives from its re-key path, and measure what
// the design claims: nothing of the user's runs before the line that asks for
// it, the shell signs exactly one start record, a warm tab's first prompt reads
// as a cold tab's (its `$?`, `$SECONDS`, history and prompt hooks), the holds
// ride out a resize's SIGWINCH, and a discarded spare writes no history.

/// The capability nonce the rig hands every shell (64 lowercase hex).
#[cfg(unix)]
const SPARE_NONCE: &str = "5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a";

/// The session id the rig names its channel files by.
#[cfg(unix)]
const SPARE_SID: &str = "s-0123456789abcdef0123";

/// A signed `633;P` mark as the shells print it.
#[cfg(unix)]
fn signed_p(prop: &str) -> String {
    format!("\x1b]633;P;{prop};id={SPARE_NONCE}\x07")
}

/// One spare's world: a home, this build's scripts, and a control dir whose
/// `rekey/<sid>` names the shell's re-key file and whose `spare/<sid>` is its
/// FIFO (made by [`Self::fifo`], or never, for a cold shell).
#[cfg(unix)]
struct SpareRig {
    _root: aterm_tempfile::TempDir,
    home: PathBuf,
    dir: PathBuf,
    rekey: PathBuf,
    fifo: PathBuf,
}

#[cfg(unix)]
impl SpareRig {
    fn new() -> Self {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let home = root.path().join("home");
        let dir = root.path().join("si").join(script_set_address());
        let ctl = root.path().join("ctl");
        for d in [&home, &ctl.join("rekey"), &ctl.join(SPARE_DIR)] {
            std::fs::create_dir_all(d).expect("rig dir");
        }
        ensure_scripts(&dir).expect("scripts");
        Self {
            rekey: ctl.join("rekey").join(SPARE_SID),
            fifo: ctl.join(SPARE_DIR).join(SPARE_SID),
            home,
            dir,
            _root: root,
        }
    }

    /// Write `name` in the home.
    fn rc(&self, name: &str, body: &str) {
        std::fs::write(self.home.join(name), body).expect("rc");
    }

    fn ran(&self, marker: &str) -> bool {
        self.home.join(marker).exists()
    }

    /// Make the spare's FIFO and hold it open read-write, as the host does for
    /// the spare's life: the shell's open never blocks, and a line written
    /// before the shell reads waits in the pipe.
    fn fifo(&self) -> std::fs::File {
        let made = Command::new("/usr/bin/mkfifo")
            .arg("-m")
            .arg("600")
            .arg(&self.fifo)
            .status()
            .expect("mkfifo");
        assert!(made.success(), "mkfifo {}", self.fifo.display());
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&self.fifo)
            .expect("hold the FIFO")
    }

    /// An interactive `shell` on a real pty, started from this build's folder the
    /// way aterm starts a tab, with the rig's nonce and re-key path. Unlike
    /// `spawn_pty_shell` it waits for nothing: a spare prints no prompt.
    fn spawn(&self, shell: ShellType, program: &str) -> LiveShell {
        self.spawn_from(shell, program, &self.dir)
    }

    fn spawn_from(&self, shell: ShellType, program: &str, dir: &Path) -> LiveShell {
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
        cmd.env("HOME", &self.home)
            .env("PATH", "/usr/bin:/bin")
            .env("TERM", "xterm-256color")
            .env("ATERM_CHILD", "1")
            .env("ATERM_SHELL_NONCE", SPARE_NONCE)
            .env("ATERM_REKEY_PATH", &self.rekey)
            .env_remove("BASH_ENV");
        for (key, value) in env_add {
            cmd.env(key, value);
        }
        if shell == ShellType::Zsh {
            cmd.env_remove("ATERM_UNSET_ZDOTDIR")
                .env("ATERM_ORIGINAL_ZDOTDIR", &self.home);
        }
        let label = match shell {
            ShellType::Zsh => "zsh",
            _ => "bash",
        };
        LiveShell::spawn(label, &mut cmd).holding(hermetic.into_home())
    }
}

#[cfg(unix)]
impl LiveShell {
    /// The shell's pid: `script`'s one child.
    fn shell_pid(&mut self) -> u32 {
        let started = std::time::Instant::now();
        loop {
            let out = Command::new("pgrep")
                .arg("-P")
                .arg(self.child.id().to_string())
                .output()
                .expect("pgrep");
            if let Some(pid) = String::from_utf8_lossy(&out.stdout)
                .lines()
                .find_map(|l| l.trim().parse().ok())
            {
                return pid;
            }
            assert!(
                started.elapsed() < Self::TIMEOUT,
                "{}: no shell under script\n{}",
                self.label,
                self.transcript()
            );
            std::thread::sleep(std::time::Duration::from_millis(15));
        }
    }

    /// Wait for the shell (and so `script`) to exit; a hang detector.
    fn wait_exit(&mut self) {
        let started = std::time::Instant::now();
        while !matches!(self.child.try_wait(), Ok(Some(_))) {
            assert!(
                started.elapsed() < Self::TIMEOUT,
                "{}: did not exit\n{}",
                self.label,
                self.transcript()
            );
            std::thread::sleep(std::time::Duration::from_millis(15));
        }
    }

    /// End a shell the test no longer needs: `script` killed, its pty closed,
    /// the shell hung up.
    fn kill_now(mut self) {
        let _ = self.child.kill();
        self.wait_exit();
    }

    fn output(&self) -> String {
        let out = self
            .stdout
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        String::from_utf8_lossy(&out).into_owned()
    }
}

/// Send `sig` (a `kill` name) to `pid`.
#[cfg(unix)]
fn signal(sig: &str, pid: u32) {
    let sent = Command::new("kill")
        .arg(format!("-{sig}"))
        .arg(pid.to_string())
        .status()
        .expect("kill");
    assert!(sent.success(), "kill -{sig} {pid}");
}

/// Write one protocol line to the spare's FIFO.
#[cfg(unix)]
fn tell(host: &mut std::fs::File, line: &str) {
    use std::io::Write as _;
    host.write_all(format!("{line}\n").as_bytes())
        .expect("write the FIFO");
}

/// A zsh and a pty: COULD NOT RUN where the host has either missing.
#[cfg(unix)]
fn spare_zsh() -> &'static str {
    live_pty_zsh()
}

/// A bash and a pty: COULD NOT RUN where the host cannot allocate a pty.
#[cfg(unix)]
fn spare_bash() -> &'static str {
    need_pty_script();
    bash_shell()
}

/// `${BASH_VERSINFO[0]}` of `bash`: the spare hold's SIGWINCH negative control
/// holds only where a signal fails the hold's FIFO open (bash 3.2).
#[cfg(unix)]
fn bash_major_version(bash: &str) -> u32 {
    let out = Command::new(bash)
        .args(["-c", "echo ${BASH_VERSINFO[0]}"])
        .output()
        .unwrap_or_else(|e| panic!("{bash} -c: {e}"));
    let text = String::from_utf8_lossy(&out.stdout);
    text.trim()
        .parse()
        .unwrap_or_else(|_| panic!("{bash}: BASH_VERSINFO[0] read {text:?}"))
}

/// The shells under test with the startup file whose run each reports.
#[cfg(unix)]
fn spare_shells() -> Vec<(ShellType, &'static str, &'static str)> {
    vec![
        (ShellType::Zsh, spare_zsh(), ".zshenv"),
        (ShellType::Bash, spare_bash(), ".bashrc"),
    ]
}

/// S1, the PARKED hold. With the FIFO present no startup file runs until a line
/// arrives: `go` runs them in view, then the shell signs `cold` and its first
/// prompt; `run` runs them unseen, then it signs `spare` and draws no prompt
/// until `go`. What was typed meanwhile waits in the tty and runs after the
/// release (S7). Negative control: with no FIFO the same shell runs its startup
/// files at once and signs `cold`, never `parked`.
#[cfg(unix)]
#[test]
fn a_spare_runs_no_startup_file_until_asked_and_signs_one_start_record() {
    for (shell, program, rc) in spare_shells() {
        let rc_body = ": > \"$HOME/rc.ran\"\nPS1='P> '\n".to_owned();
        // Negative control: no FIFO, a cold shell.
        let rig = SpareRig::new();
        rig.rc(rc, &rc_body);
        let mut sh = rig.spawn(shell, program);
        let start = sh.wait_for_after(&signed_p("AtermStart=cold"), 0);
        sh.wait_for_after("\x1b]133;A", start);
        assert!(rig.ran("rc.ran"), "{program}: a cold shell runs its rc");
        assert!(
            !sh.output().contains("AtermSpare="),
            "{program}: a cold shell never says it is parked\n{}",
            sh.transcript()
        );
        sh.finish();

        // `go`: in view, as a cold shell started now.
        let rig = SpareRig::new();
        rig.rc(rc, &rc_body);
        let mut host = rig.fifo();
        let mut sh = rig.spawn(shell, program);
        let parked = sh.wait_for_after(&signed_p("AtermSpare=parked"), 0);
        assert!(!rig.ran("rc.ran"), "{program}: parked, no startup file ran");
        tell(&mut host, "go");
        let start = sh.wait_for_after(&signed_p("AtermStart=cold"), parked);
        sh.wait_for_after("\x1b]133;A", start);
        assert!(rig.ran("rc.ran"), "{program}: released, the rc ran");
        assert!(
            !stdout_between(&sh, 0, start).contains("\x1b]133;A"),
            "{program}: the start record comes before the first prompt\n{}",
            sh.transcript()
        );
        sh.finish();

        // `run`: unseen, then held at the first prompt hook until `go`.
        let rig = SpareRig::new();
        rig.rc(rc, &rc_body);
        let mut host = rig.fifo();
        let mut sh = rig.spawn(shell, program);
        let parked = sh.wait_for_after(&signed_p("AtermSpare=parked"), 0);
        tell(&mut host, "run");
        let spare = sh.wait_for_after(&signed_p("AtermStart=spare"), parked);
        assert!(rig.ran("rc.ran"), "{program}: told to run, the rc ran");
        // What is typed before the release: the kernel echoes it now, and no
        // prompt was drawn before it.
        sh.send("echo TYPED$((40+2))");
        let echoed = sh.wait_for_after("echo TYPED$((40+2))", spare);
        assert!(
            !stdout_between(&sh, spare, echoed).contains("\x1b]133;A"),
            "{program}: no prompt while held\n{}",
            sh.transcript()
        );
        tell(&mut host, "go");
        let prompt = sh.wait_for_after("\x1b]133;A", echoed);
        sh.wait_for_after("TYPED42", prompt);
        assert_eq!(
            sh.output().matches("AtermStart=").count(),
            1,
            "{program}: one start record\n{}",
            sh.transcript()
        );
        sh.finish();
    }
}

/// S2, zsh: a warm tab's first prompt reads as a cold tab's. The `$?` an rc
/// ending in `false` leaves; a hook the rc put ahead of aterm's re-run after
/// the release, silenced
/// (it reads what the file holds now, and its stderr appears once, from its run
/// at spare time). The flags name what would keep a generation from running
/// unseen: a `sched` event, a job, a periodic hook.
#[cfg(unix)]
#[test]
fn a_warm_zsh_first_prompt_reads_as_a_cold_one() {
    let zsh = spare_zsh();
    let rc = "PS1='ST=%? P> '\n\
              ahead() { cat \"$HOME/f\" > \"$HOME/seen\" 2>/dev/null; print -u2 HOOKERR$((40+2)); }\n\
              precmd_functions=(ahead $precmd_functions)\n\
              false\n";
    // Cold: the reference prompt.
    let rig = SpareRig::new();
    rig.rc(".zshrc", rc);
    std::fs::write(rig.home.join("f"), "before").unwrap();
    let mut sh = rig.spawn(ShellType::Zsh, zsh);
    let a = sh.wait_for_after("\x1b]133;A", 0);
    let cold_prompt = sh.wait_for_after("P> ", a);
    let cold = stdout_between(&sh, a, cold_prompt);
    sh.finish();

    let rig = SpareRig::new();
    rig.rc(".zshrc", rc);
    std::fs::write(rig.home.join("f"), "before").unwrap();
    let mut host = rig.fifo();
    let mut sh = rig.spawn(ShellType::Zsh, zsh);
    let parked = sh.wait_for_after(&signed_p("AtermSpare=parked"), 0);
    tell(&mut host, "run");
    let spare = sh.wait_for_after(&signed_p("AtermStart=spare"), parked);
    assert!(
        sh.output()
            .contains(&signed_p("AtermStartFlags=pre:1,s:0,j:0,r:0")),
        "one hook ahead of aterm's\n{}",
        sh.transcript()
    );
    std::fs::write(rig.home.join("f"), "after").unwrap();
    tell(&mut host, "go");
    let a = sh.wait_for_after("\x1b]133;A", spare);
    let warm_prompt = sh.wait_for_after("P> ", a);
    assert_eq!(
        stdout_between(&sh, a, warm_prompt),
        cold,
        "the warm prompt's `$?` is the cold one's"
    );
    assert_eq!(
        std::fs::read_to_string(rig.home.join("seen")).unwrap(),
        "after",
        "the hook ahead of aterm's re-ran after the release"
    );
    assert_eq!(
        sh.stderr_text().matches("HOOKERR42").count() + sh.output().matches("HOOKERR42").count(),
        1,
        "the re-run is silenced\n{}",
        sh.transcript()
    );
    sh.finish();

    // The flags: a pending `sched` event, a job and a periodic hook.
    let rig = SpareRig::new();
    rig.rc(
        ".zshrc",
        "sched +600 :\nsleep 600 &\nPERIOD=600\nperiodic() { :; }\n",
    );
    let mut host = rig.fifo();
    let mut sh = rig.spawn(ShellType::Zsh, zsh);
    let parked = sh.wait_for_after(&signed_p("AtermSpare=parked"), 0);
    tell(&mut host, "run");
    sh.wait_for_after(&signed_p("AtermStart=spare"), parked);
    assert!(
        sh.output()
            .contains(&signed_p("AtermStartFlags=pre:0,s:1,j:1,r:1")),
        "{}",
        sh.transcript()
    );
    let pid = sh.shell_pid();
    sh.kill_now();
    let _ = pid;
}

/// The history a shell holds and leaves, read back the same way for a cold tab
/// and a warm one: the in-memory list with its event numbers, `$HISTCMD`, the
/// prompt's `%!`, then the file after `exit`. Timestamps are blanked.
#[cfg(unix)]
fn zsh_history_reading(rig: &SpareRig, warm: bool, append_during_hold: &str) -> String {
    let zsh = spare_zsh();
    let mut host = warm.then(|| rig.fifo());
    let mut sh = rig.spawn(ShellType::Zsh, zsh);
    let at = if let Some(host) = host.as_mut() {
        let parked = sh.wait_for_after(&signed_p("AtermSpare=parked"), 0);
        tell(host, "run");
        let spare = sh.wait_for_after(&signed_p("AtermStart=spare"), parked);
        if !append_during_hold.is_empty() {
            use std::io::Write as _;
            std::fs::OpenOptions::new()
                .append(true)
                .open(rig.home.join("hist"))
                .and_then(|mut f| f.write_all(append_during_hold.as_bytes()))
                .expect("append history");
        }
        tell(host, "go");
        sh.wait_for_after("\x1b]133;A", spare)
    } else {
        sh.wait_for_after("\x1b]133;A", 0)
    };
    sh.wait_for_zle(at);
    let from = sh.output().len();
    // `DU""MP`: the listing quotes this line, and must not hold the marker.
    sh.send("print -r -- \"DU\"\"MP $HISTCMD ${(%):-%!}\"; fc -l 1; print -r -- END$((40+2))");
    let end = sh.wait_for_after("END42", from);
    let dump_from = sh.output()[..end].rfind("DUMP ").expect("dump");
    let dump = stdout_between(&sh, dump_from, end);
    sh.finish();
    let file = std::fs::read_to_string(rig.home.join("hist")).unwrap_or_default();
    let blank = |text: &str| -> String {
        text.lines()
            .map(|l| {
                // `: <epoch>:<elapsed>;cmd` (EXTENDED_HISTORY) loses its stamp.
                match l.strip_prefix(": ").and_then(|r| r.split_once(';')) {
                    Some((_, cmd)) => format!(": T;{cmd}"),
                    None => l.to_owned(),
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    format!("{}\n--- file ---\n{}", dump.replace('\r', ""), blank(&file))
}

/// S3, zsh: the history a warm tab holds after its release is exactly a cold
/// tab's, for 8 history options taken 0 to 3 at a time (93 combinations), with
/// the file appended during the hold and left unchanged. The release reads the
/// file again as a cold start reads it (`fc -p`), with the stale list pushed
/// under at SAVEHIST 0, so it is never written back.
#[cfg(unix)]
#[test]
fn a_warm_zsh_holds_and_leaves_the_history_a_cold_one_does() {
    // COULD NOT RUN here, before any worker starts, where the host lacks zsh or a pty.
    spare_zsh();
    const OPTS: [&str; 8] = [
        "extended_history",
        "inc_append_history",
        "share_history",
        "hist_ignore_dups",
        "hist_ignore_all_dups",
        "hist_save_no_dups",
        "hist_expire_dups_first",
        "hist_find_no_dups",
    ];
    let mut combos: Vec<Vec<&str>> = vec![Vec::new()];
    for (i, a) in OPTS.iter().enumerate() {
        combos.push(vec![*a]);
        for (j, b) in OPTS.iter().enumerate().skip(i + 1) {
            combos.push(vec![*a, *b]);
            for c in OPTS.iter().skip(j + 1) {
                combos.push(vec![*a, *b, *c]);
            }
        }
    }
    assert_eq!(combos.len(), 93);
    let seed = ": 1700000000:0;echo one\n: 1700000001:0;echo two\n: 1700000002:0;echo two\n";
    let appended = ": 1700000003:0;echo three\n: 1700000004:0;echo one\n";
    let failures = std::sync::Mutex::new(Vec::new());
    let next = std::sync::atomic::AtomicUsize::new(0);
    std::thread::scope(|scope| {
        for _ in 0..3 {
            scope.spawn(|| {
                loop {
                    let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let Some(opts) = combos.get(i) else { break };
                    for append in [appended, ""] {
                        let rc = format!(
                            "HISTFILE=\"$HOME/hist\"\nHISTSIZE=50\nSAVEHIST=50\n{}PS1='P> '\n",
                            opts.iter()
                                .map(|o| format!("setopt {o}\n"))
                                .collect::<String>()
                        );
                        let warm_rig = SpareRig::new();
                        warm_rig.rc(".zshrc", &rc);
                        std::fs::write(warm_rig.home.join("hist"), seed).unwrap();
                        let warm = zsh_history_reading(&warm_rig, true, append);
                        let cold_rig = SpareRig::new();
                        cold_rig.rc(".zshrc", &rc);
                        std::fs::write(cold_rig.home.join("hist"), format!("{seed}{append}"))
                            .unwrap();
                        let cold = zsh_history_reading(&cold_rig, false, "");
                        if warm != cold {
                            failures.lock().unwrap().push(format!(
                                "{opts:?} appended={}:\n--- warm ---\n{warm}\n--- cold ---\n{cold}",
                                !append.is_empty()
                            ));
                        }
                    }
                }
            });
        }
    });
    let failures = failures.into_inner().unwrap();
    assert!(
        failures.is_empty(),
        "{} of 186 differ:\n{}",
        failures.len(),
        failures.join("\n\n")
    );
}

/// A warm zsh's release reads the history file again only when the file moved
/// during the hold (its mtime, size or inode), so a release the file did not
/// move pays no read. Seen through a rewrite of the same size in place: with the
/// file's mtime put back, the list after the release is the one the shell read
/// at its start; with it moved, the one on disk. The rig's home has no atpkg
/// hook, so zsh/stat is not yet loaded at the hold (a session shell defers it to
/// the first hook it stamps); a hold that does not load it itself has no stamp
/// and reads the file again at every release, which fails the first case.
#[cfg(unix)]
#[test]
fn a_warm_zsh_reads_its_history_again_only_when_the_file_moved() {
    use std::io::Write as _;
    let zsh = spare_zsh();
    for moved in [false, true] {
        let rig = SpareRig::new();
        rig.rc(
            ".zshrc",
            "HISTFILE=\"$HOME/hist\"\nHISTSIZE=50\nSAVEHIST=50\nPS1='P> '\n",
        );
        let hist = rig.home.join("hist");
        std::fs::write(&hist, "echo AAAA\n").expect("seed history");
        let mut host = rig.fifo();
        let mut sh = rig.spawn(ShellType::Zsh, zsh);
        let parked = sh.wait_for_after(&signed_p("AtermSpare=parked"), 0);
        tell(&mut host, "run");
        let spare = sh.wait_for_after(&signed_p("AtermStart=spare"), parked);
        let was = std::fs::metadata(&hist)
            .and_then(|m| m.modified())
            .expect("history mtime");
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(&hist)
            .expect("open history");
        (&file).write_all(b"echo BBBB\n").expect("rewrite history");
        let mtime = if moved {
            was + std::time::Duration::from_secs(5)
        } else {
            was
        };
        file.set_modified(mtime).expect("history mtime");
        drop(file);
        tell(&mut host, "go");
        let at = sh.wait_for_after("\x1b]133;A", spare);
        sh.wait_for_zle(at);
        let from = sh.output().len();
        sh.send("fc -ln 1; print -r -- END$((40+2))");
        let end = sh.wait_for_after("END42", from);
        let listed = stdout_between(&sh, from, end);
        sh.finish();
        let (held, not) = if moved {
            ("echo BBBB", "echo AAAA")
        } else {
            ("echo AAAA", "echo BBBB")
        };
        assert!(
            listed.contains(held) && !listed.contains(not),
            "moved={moved}: the list after the release holds {held:?}, not {not:?}:\n{listed}"
        );
    }
}

/// The bash twin of [`zsh_history_reading`].
#[cfg(unix)]
fn bash_history_reading(rig: &SpareRig, warm: bool, append_during_hold: &str) -> String {
    let bash = spare_bash();
    let mut host = warm.then(|| rig.fifo());
    let mut sh = rig.spawn(ShellType::Bash, bash);
    if let Some(host) = host.as_mut() {
        let parked = sh.wait_for_after(&signed_p("AtermSpare=parked"), 0);
        tell(host, "run");
        let spare = sh.wait_for_after(&signed_p("AtermStart=spare"), parked);
        if !append_during_hold.is_empty() {
            use std::io::Write as _;
            std::fs::OpenOptions::new()
                .append(true)
                .open(rig.home.join("hist"))
                .and_then(|mut f| f.write_all(append_during_hold.as_bytes()))
                .expect("append history");
        }
        tell(host, "go");
        sh.wait_for_after("\x1b]133;A", spare);
    } else {
        sh.wait_for_after("\x1b]133;A", 0);
    }
    let from = sh.output().len();
    sh.send("echo \"DU\"\"MP $HISTCMD\"; history; echo END$((40+2))");
    let end = sh.wait_for_after("END42", from);
    let dump_from = sh.output()[..end].rfind("DUMP ").expect("dump");
    let dump = stdout_between(&sh, dump_from, end);
    sh.finish();
    let file = std::fs::read_to_string(rig.home.join("hist")).unwrap_or_default();
    let blank: String = file
        .lines()
        .map(|l| if l.starts_with('#') { "#T" } else { l })
        .collect::<Vec<_>>()
        .join("\n");
    // `history`'s own timestamps (HISTTIMEFORMAT) are the clock's: blanked.
    let dump: String = dump
        .replace('\r', "")
        .lines()
        .map(|l| match l.split_once(" @") {
            Some((head, _)) => format!("{head} @T"),
            None => l.to_owned(),
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!("{dump}\n--- file ---\n{blank}")
}

/// S4, bash: the same for four history knobs in all 16 combinations, plus a
/// shell with HISTFILE unset. bash 3.2 has no fork-free stat, so the release
/// always reads the list again (`history -c; history -r`), which is what a cold
/// start reads from the same file.
#[cfg(unix)]
#[test]
fn a_warm_bash_holds_and_leaves_the_history_a_cold_one_does() {
    // COULD NOT RUN here, before any worker starts, where the host lacks a pty.
    spare_bash();
    const KNOBS: [&str; 4] = [
        "shopt -s histappend\n",
        "HISTCONTROL=ignoreboth\n",
        "HISTTIMEFORMAT='@%s '\n",
        "HISTFILESIZE=4\n",
    ];
    let seed = "echo one\necho two\necho two\n";
    let appended = "echo three\necho one\n";
    let mut cases: Vec<String> = (0..16u32)
        .map(|mask| {
            let knobs: String = (0..4)
                .filter(|b| mask & (1 << b) != 0)
                .map(|b| KNOBS[b])
                .collect();
            format!("HISTFILE=\"$HOME/hist\"\nHISTSIZE=50\n{knobs}PS1='P> '\n")
        })
        .collect();
    cases.push("unset HISTFILE\nPS1='P> '\n".to_owned());
    let failures = std::sync::Mutex::new(Vec::new());
    let next = std::sync::atomic::AtomicUsize::new(0);
    std::thread::scope(|scope| {
        for _ in 0..2 {
            scope.spawn(|| {
                loop {
                    let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let Some(rc) = cases.get(i) else { break };
                    for append in [appended, ""] {
                        let warm_rig = SpareRig::new();
                        warm_rig.rc(".bashrc", rc);
                        std::fs::write(warm_rig.home.join("hist"), seed).unwrap();
                        let warm = bash_history_reading(&warm_rig, true, append);
                        let cold_rig = SpareRig::new();
                        cold_rig.rc(".bashrc", rc);
                        std::fs::write(cold_rig.home.join("hist"), format!("{seed}{append}"))
                            .unwrap();
                        let cold = bash_history_reading(&cold_rig, false, "");
                        if warm != cold {
                            failures.lock().unwrap().push(format!(
                                "{rc:?} appended={}:\n--- warm ---\n{warm}\n--- cold ---\n{cold}",
                                !append.is_empty()
                            ));
                        }
                    }
                }
            });
        }
    });
    let failures = failures.into_inner().unwrap();
    assert!(
        failures.is_empty(),
        "{} of 34 differ:\n{}",
        failures.len(),
        failures.join("\n\n")
    );
}

/// S5: both holds ride out a resize. Ten SIGWINCHes (what each TIOCSWINSZ on
/// the master sends the foreground group) at the parked hold and at the warm
/// hold release neither; `go` releases each. Negative control: bash's warm hold
/// with its loop cut and no writer held is released by the signals, as the
/// design's probe found (bash 3.2's `read < fifo` fails when a signal
/// interrupts the open, and the shell runs on; bash 5 restarts the open).
#[cfg(unix)]
#[test]
fn a_spare_hold_survives_sigwinch_and_only_go_releases_it() {
    for (shell, program, _) in spare_shells() {
        let rig = SpareRig::new();
        rig.rc(
            if shell == ShellType::Zsh {
                ".zshrc"
            } else {
                ".bashrc"
            },
            ": > \"$HOME/rc.ran\"\n",
        );
        let mut host = rig.fifo();
        let mut sh = rig.spawn(shell, program);
        let parked = sh.wait_for_after(&signed_p("AtermSpare=parked"), 0);
        let pid = sh.shell_pid();
        for _ in 0..10 {
            signal("WINCH", pid);
        }
        // `run` releases the parked hold; had a signal released it, the rc
        // would have run before the line and the start record would say so.
        tell(&mut host, "run");
        let spare = sh.wait_for_after(&signed_p("AtermStart=spare"), parked);
        assert!(
            !sh.output().contains(&signed_p("AtermStart=cold")),
            "{program}: the parked hold held through SIGWINCH\n{}",
            sh.transcript()
        );
        for _ in 0..10 {
            signal("WINCH", pid);
        }
        sh.send("echo HELD$((40+2))");
        let echoed = sh.wait_for_after("echo HELD$((40+2))", spare);
        assert!(
            !stdout_between(&sh, spare, echoed).contains("\x1b]133;A"),
            "{program}: the warm hold held through SIGWINCH\n{}",
            sh.transcript()
        );
        tell(&mut host, "go");
        let a = sh.wait_for_after("\x1b]133;A", echoed);
        sh.wait_for_after("HELD42", a);
        sh.finish();
    }

    // Negative control: bash's warm hold without the loop or the host's writer.
    // It needs a bash whose `read < fifo` fails when a signal interrupts the
    // open: macOS's /bin/bash 3.2. Bash 5 restarts the open (Linux's 5.2 and
    // Homebrew's 5.3 on macOS: sixty seconds of SIGWINCH every 20 ms never
    // released it), so under it the cut hold cannot be released and there is
    // nothing to control — the loop exists for 3.2's hold.
    let bash = spare_bash();
    if bash_major_version(bash) >= 5 {
        return;
    }
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let cut = folder_with_edits(
        root.path(),
        "00000000000000ab",
        &[(
            "bash",
            scripts::BASH,
            &[(
                "        else\n            __aterm_n=$((__aterm_n + 1))\n            { [[ $__aterm_n -lt 50 ]] && [[ -p $__aterm_spare_fifo ]]; } || exit 0\n        fi\n    done 2>/dev/null\n",
                "        fi\n        break\n    done 2>/dev/null\n",
            )],
        )],
    );
    let rig = SpareRig::new();
    rig.rc(".bashrc", "PS1='P> '\n");
    let mut host = rig.fifo();
    let mut sh = rig.spawn_from(ShellType::Bash, bash, &cut);
    let parked = sh.wait_for_after(&signed_p("AtermSpare=parked"), 0);
    // `run`, and the host lets go of its writer at once: the parked hold still
    // reads the line it holds, and the warm hold's open waits for a writer.
    tell(&mut host, "run");
    drop(host);
    let spare = sh.wait_for_after(&signed_p("AtermStart=spare"), parked);
    let pid = sh.shell_pid();
    let mut released = false;
    let started = std::time::Instant::now();
    while !released {
        signal("WINCH", pid);
        released = sh.output()[spare..].contains("\x1b]133;A");
        assert!(
            started.elapsed() < LiveShell::TIMEOUT,
            "the cut hold was never released by SIGWINCH\n{}",
            sh.transcript()
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    sh.kill_now();
}

/// S6: a spare discarded at either hold writes no history (its HISTFILE is
/// byte for byte what it was), and a shell whose FIFO is gone ends itself.
#[cfg(unix)]
#[test]
fn a_discarded_spare_writes_no_history_and_ends_without_its_fifo() {
    for (shell, program, _) in spare_shells() {
        let (rc_name, rc) = match shell {
            ShellType::Zsh => (
                ".zshrc",
                "HISTFILE=\"$HOME/hist\"\nHISTSIZE=50\nSAVEHIST=50\nsetopt inc_append_history\n",
            ),
            _ => (
                ".bashrc",
                "HISTFILE=\"$HOME/hist\"\nHISTSIZE=50\nshopt -s histappend\n",
            ),
        };
        let seed = "echo kept\n";
        for at_warm in [false, true] {
            let rig = SpareRig::new();
            rig.rc(rc_name, rc);
            std::fs::write(rig.home.join("hist"), seed).unwrap();
            let mut host = rig.fifo();
            let mut sh = rig.spawn(shell, program);
            let parked = sh.wait_for_after(&signed_p("AtermSpare=parked"), 0);
            if at_warm {
                tell(&mut host, "run");
                sh.wait_for_after(&signed_p("AtermStart=spare"), parked);
            }
            let pid = sh.shell_pid();
            signal("HUP", pid);
            sh.wait_exit();
            assert_eq!(
                std::fs::read_to_string(rig.home.join("hist")).unwrap(),
                seed,
                "{program}: a spare hung up at the {} hold writes no history",
                if at_warm { "warm" } else { "parked" }
            );
        }
        // The FIFO gone and the host's writer closed: the hold ends the shell.
        let rig = SpareRig::new();
        rig.rc(rc_name, rc);
        let host = rig.fifo();
        let mut sh = rig.spawn(shell, program);
        sh.wait_for_after(&signed_p("AtermSpare=parked"), 0);
        std::fs::remove_file(&rig.fifo).unwrap();
        drop(host);
        sh.wait_exit();
    }
}

/// S8: a warm tab's `$SECONDS` carries on from its rc's run, not the hold's.
/// An rc that sets it to 1000 reads at least 1000 after the release in both
/// shells (a release that restarted the clock reads less). zsh also proves
/// the hold's own time is left out, with no wall-clock bound: from the rc's
/// realtime stamp taken just before it set the clock (`rc_at`), the start
/// record seen (`marked`), the release written (`released`) and the shell's
/// realtime at the read (`now`), a clock restored at the release reads at most
/// `(marked - rc_at) + (now - released)` past 1000 however loaded the host is,
/// and one that ran through the hold reads `released - marked` (the hold this
/// test keeps) more.
#[cfg(unix)]
#[test]
fn a_warm_spare_carries_seconds_on_from_its_rc_run() {
    let wall = || {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("after the epoch")
            .as_secs_f64()
    };
    for (shell, program, _) in spare_shells() {
        let (rc_name, rc) = match shell {
            ShellType::Zsh => (
                ".zshrc",
                "zmodload zsh/datetime\nprint -r -- $EPOCHREALTIME > \"$HOME/rc-at\"\n\
                 typeset -F SECONDS=1000\nPS1='P> '\n",
            ),
            _ => (".bashrc", "SECONDS=1000\nPS1='P> '\n"),
        };
        let rig = SpareRig::new();
        rig.rc(rc_name, rc);
        let mut host = rig.fifo();
        let mut sh = rig.spawn(shell, program);
        let parked = sh.wait_for_after(&signed_p("AtermSpare=parked"), 0);
        tell(&mut host, "run");
        let spare = sh.wait_for_after(&signed_p("AtermStart=spare"), parked);
        let marked = wall();
        // A hold whose time a clock that ran through it would carry.
        std::thread::sleep(std::time::Duration::from_millis(300));
        let released = wall();
        tell(&mut host, "go");
        let a = sh.wait_for_after("\x1b]133;A", spare);
        sh.wait_for_after("P> ", a);
        let from = sh.output().len();
        sh.send("echo SECS=$SECONDS; echo NOW=${EPOCHREALTIME-}");
        let end = sh.wait_for_after("\x1b]133;A", from);
        let secs: f64 = sh
            .value_before("SECS=", end)
            .trim()
            .parse()
            .unwrap_or_else(|e| panic!("{program}: SECONDS ({e})\n{}", sh.transcript()));
        assert!(
            secs >= 1000.0,
            "{program}: SECONDS carried on from the rc's 1000, not restarted: {secs}"
        );
        if shell == ShellType::Zsh {
            let now: f64 = sh.value_before("NOW=", end).trim().parse().expect("now");
            let rc_at: f64 = std::fs::read_to_string(rig.home.join("rc-at"))
                .expect("rc-at")
                .trim()
                .parse()
                .expect("rc-at");
            // A millisecond for the six digits the stamps print.
            let bound = (marked - rc_at) + (now - released) + 0.001;
            assert!(
                secs - 1000.0 <= bound,
                "{program}: SECONDS ran through the hold: {} s past 1000, at most {bound} s \
                 without it",
                secs - 1000.0
            );
        }
        sh.finish();
    }
}
