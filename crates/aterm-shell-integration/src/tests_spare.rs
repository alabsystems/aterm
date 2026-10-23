// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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
    root: std::sync::Arc<std::sync::Mutex<Option<aterm_tempfile::TempDir>>>,
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
        for d in [home.clone(), ctl.join("rekey"), ctl.join(SPARE_DIR)] {
            std::fs::create_dir_all(d).expect("rig dir");
        }
        ensure_scripts(&dir).expect("scripts");
        Self {
            rekey: ctl.join("rekey").join(SPARE_SID),
            fifo: ctl.join(SPARE_DIR).join(SPARE_SID),
            home,
            dir,
            root: std::sync::Arc::new(std::sync::Mutex::new(Some(root))),
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
        self.make_fifo();
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&self.fifo)
            .expect("hold the FIFO")
    }

    /// Make the spare's FIFO and hold nothing of it: a hold's open then waits
    /// for a writer ([`Self::writer_met_by`]).
    fn make_fifo(&self) {
        let made = Command::new("/usr/bin/mkfifo")
            .arg("-m")
            .arg("600")
            .arg(&self.fifo)
            .status()
            .expect("mkfifo");
        assert!(made.success(), "mkfifo {}", self.fifo.display());
    }

    /// A writer of the FIFO [`Self::make_fifo`] made, opened as `sh`'s hold opens
    /// it to read: a FIFO's open for writing waits for a reader, so when this
    /// returns the hold has the FIFO open, and what is written and closed now is
    /// what it reads, then end of file. Its parked mark is printed before that
    /// open, so a writer let go of on seeing the mark could close before the
    /// open and leave it waiting for a writer for good. A hang detector: a hold
    /// that never opens the FIFO fails at [`LiveShell::TIMEOUT`].
    fn writer_met_by(&self, sh: &LiveShell) -> std::fs::File {
        let path = self.fifo.clone();
        let (met, meeting) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = met.send(std::fs::OpenOptions::new().write(true).open(path));
        });
        match meeting.recv_timeout(LiveShell::TIMEOUT) {
            Ok(writer) => writer.expect("open the FIFO to write"),
            Err(_) => {
                // A reader of this process's own lets the waiting open go.
                use std::os::unix::fs::OpenOptionsExt as _;
                let _ = std::fs::OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_NONBLOCK)
                    .open(&self.fifo);
                panic!(
                    "{}: the hold never opened its FIFO\n{}",
                    sh.label,
                    sh.transcript()
                );
            }
        }
    }

    /// An interactive `shell` on a real pty, started from this build's folder the
    /// way aterm starts a tab, with the rig's nonce and re-key path. Unlike
    /// `spawn_pty_shell` it waits for nothing: a spare prints no prompt.
    fn spawn(&self, shell: ShellType, program: &str) -> LiveShell {
        self.spawn_from(shell, program, &self.dir)
    }

    /// An rc that leaves a job running lends its private PID file to the
    /// shell guard before the first wait can fail. Unknown custody retains
    /// this entire rig as well as the shell's hermetic home.
    fn spawn_with_job(&self, shell: ShellType, program: &str, host: &std::fs::File) -> LiveShell {
        let release = host.try_clone().expect("duplicate the rig's own FIFO");
        let mut sh = self.spawn(shell, program);
        sh.job = Some(ShellJob {
            pid_file: self.home.join("job"),
            root: std::sync::Arc::clone(&self.root),
            script: OwnedShellPid::read(sh.child.id()),
            release,
        });
        sh
    }

    /// [`Self::spawn`] started in folder `cwd`, as a tab asked for there, with
    /// `env` added to what it inherits.
    fn spawn_in(
        &self,
        shell: ShellType,
        program: &str,
        cwd: &Path,
        env: &[(&str, &Path)],
    ) -> LiveShell {
        self.spawn_with(shell, program, &self.dir, Some(cwd), env)
    }

    fn spawn_from(&self, shell: ShellType, program: &str, dir: &Path) -> LiveShell {
        self.spawn_with(shell, program, dir, None, &[])
    }

    fn spawn_with(
        &self,
        shell: ShellType,
        program: &str,
        dir: &Path,
        cwd: Option<&Path>,
        env: &[(&str, &Path)],
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
        let mut cmd = pty_command(&argv);
        if let Some(cwd) = cwd {
            cmd.current_dir(cwd);
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
            .env("ATERM_REKEY_PATH", &self.rekey);
        for (key, value) in env_add {
            cmd.env(key, value);
        }
        for (key, value) in env {
            cmd.env(key, value);
        }
        if shell == ShellType::Fish {
            cmd.env("XDG_CONFIG_HOME", self.home.join(".config"))
                .env("XDG_DATA_HOME", self.home.join(".local").join("share"))
                .env(
                    "XDG_DATA_DIRS",
                    format!(
                        "{}:{}",
                        dir.join("fish-xdg").display(),
                        hermetic_data(hermetic.home()).display()
                    ),
                )
                .env("TERM", "dumb");
        }
        if shell == ShellType::Zsh {
            cmd.env_remove("ATERM_UNSET_ZDOTDIR")
                .env("ATERM_ORIGINAL_ZDOTDIR", &self.home);
        }
        let label = match shell {
            ShellType::Zsh => "zsh",
            ShellType::Fish => "fish",
            _ => "bash",
        };
        LiveShell::spawn_on_pty(label, &mut cmd).holding(hermetic.into_home())
    }

    /// Write `name` under fish's config dir (`$XDG_CONFIG_HOME/fish`).
    fn fish_rc(&self, name: &str, body: &str) {
        let path = self.home.join(".config").join("fish").join(name);
        std::fs::create_dir_all(path.parent().expect("a parent")).expect("fish config dir");
        std::fs::write(path, body).expect("fish rc");
    }
}

#[cfg(unix)]
impl LiveShell {
    /// The shell's pid: the child on its own pty.
    fn shell_pid(&mut self) -> u32 {
        self.child.id()
    }

    /// Wait for the shell to exit; a hang detector.
    fn wait_exit(&mut self) {
        self.wait_exit_status("did not exit");
    }

    /// End a shell the test no longer needs: killed and reaped.
    fn kill_now(mut self) {
        if self.job.is_some() {
            self.stop_owned_job().expect("owned background job retired");
            return;
        }
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
    bash_shell()
}

/// `${BASH_VERSINFO[0]}` of `bash`: the spare hold's SIGWINCH negative control
/// holds only where a signal fails the hold's FIFO open (bash 3.2).
#[cfg(unix)]
fn bash_major_version(bash: &str) -> u32 {
    let out = shell_command(bash)
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

        let rig = SpareRig::new();
        rig.rc(rc, &rc_body);
        let mut host = rig.fifo();
        let mut sh = rig.spawn(shell, program);
        let parked = sh.wait_for_after(&signed_p("AtermSpare=parked"), 0);
        tell(&mut host, "run");
        let spare = sh.wait_for_after(&signed_p("AtermStart=spare"), parked);
        assert!(rig.ran("rc.ran"), "{program}: told to run, the rc ran");
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

/// THE PARKED RETARGET (2026-10-18): a parked shell started in one folder and
/// told `go <folder>` reads, at its first prompt, as a cold shell started in
/// that folder: its `$PWD`, the folder its commands run in (`pwd -P`), and its
/// `$OLDPWD`, set or not and handed to a command or not, before and after its
/// first `cd` (zsh starts with `$OLDPWD` at `$PWD`; bash with it unset but
/// exported, or the one it inherited). NEGATIVE CONTROL: a plain `go` leaves
/// it in the folder it was started in, even with a `__aterm_spare_cd` in the
/// environment it inherited: only a line told names a folder.
#[cfg(unix)]
#[test]
fn a_parked_shell_told_a_folder_reads_as_a_cold_shell_started_there() {
    const PROBE: &str = "printf 'RD=%s|%s|%s|%s|' \"$PWD\" \"$(pwd -P)\" \"${OLDPWD-unset}\" \
         \"$(env | grep -c '^OLDPWD=')\"; cd /; printf '%s|%s|E%s\\n' \"${OLDPWD-unset}\" \
         \"$(env | grep -c '^OLDPWD=')\" \"$((40+2))\"";
    for (shell, program, rc) in spare_shells() {
        let scratch = aterm_tempfile::tempdir().expect("tempdir");
        let root = std::fs::canonicalize(scratch.path()).expect("canonical scratch");
        let (there, elsewhere) = (root.join("there"), root.join("elsewhere"));
        for d in [&there, &elsewhere] {
            std::fs::create_dir(d).expect("folder");
        }
        let reading = |release: Option<&str>, env: &[(&str, &Path)]| -> String {
            let rig = SpareRig::new();
            rig.rc(rc, ": rc ran\n");
            let mut host = release.map(|_| rig.fifo());
            let start = if release.is_some() {
                &elsewhere
            } else {
                &there
            };
            let mut sh = rig.spawn_in(shell, program, start, env);
            let from = match (host.as_mut(), release) {
                (Some(host), Some(line)) => {
                    let parked = sh.wait_for_after(&signed_p("AtermSpare=parked"), 0);
                    tell(host, line);
                    parked
                }
                _ => 0,
            };
            let a = sh.wait_for_after("\x1b]133;A", from);
            let prompt = sh.wait_for_after("\x1b]133;B", a);
            sh.send(PROBE);
            let end = sh.wait_for_after("|E42", prompt);
            let value = sh.value_before("RD=", end);
            sh.finish();
            value
        };
        let cold = reading(None, &[]);
        assert!(
            cold.starts_with(&format!("{0}|{0}|", there.display())),
            "{program}: the cold reference starts there: {cold}"
        );
        assert_eq!(
            reading(Some(&format!("go {}", there.display())), &[]),
            cold,
            "{program}: retargeted, it reads as the cold shell"
        );
        assert_ne!(
            reading(Some("go"), &[]),
            cold,
            "{program}: a plain release stays where it started"
        );
        assert_ne!(
            reading(Some("go"), &[("__aterm_spare_cd", &there)]),
            cold,
            "{program}: an inherited variable is no folder told"
        );
    }
}

/// S2, zsh: a warm tab's first prompt reads as a cold tab's. The `$?` an rc
/// ending in `false` leaves; a hook the rc put ahead of aterm's re-run after
/// the release, silenced
/// (it reads what the file holds now, and its stderr appears once, from its run
/// at spare time). A history parameter of the rc's is readonly (a hardened rc:
/// SAVEHIST, which the hold zeroes, then HISTFILE, which `fc -p` sets) and its
/// history file is not there yet, so the release would read it again: the hold
/// and the release leave them as they are. The flags name what would keep a
/// generation from running unseen: a `sched` event, a job, a periodic hook.
#[cfg(unix)]
#[test]
fn a_warm_zsh_first_prompt_reads_as_a_cold_one() {
    let zsh = spare_zsh();
    for ro in ["SAVEHIST", "HISTFILE"] {
        let rc = format!(
            "PS1='ST=%? P> '\nHISTFILE=\"$HOME/hist\"\ntypeset -r {ro}\n\
             ahead() {{ cat \"$HOME/f\" > \"$HOME/seen\" 2>/dev/null; print -u2 HOOKERR$((40+2)); }}\n\
             precmd_functions=(ahead $precmd_functions)\n\
             false\n"
        );
        let rig = SpareRig::new();
        rig.rc(".zshrc", &rc);
        std::fs::write(rig.home.join("f"), "before").unwrap();
        let mut sh = rig.spawn(ShellType::Zsh, zsh);
        let a = sh.wait_for_after("\x1b]133;A", 0);
        let cold_prompt = sh.wait_for_after("P> ", a);
        let cold = stdout_between(&sh, a, cold_prompt);
        sh.finish();

        let rig = SpareRig::new();
        rig.rc(".zshrc", &rc);
        std::fs::write(rig.home.join("f"), "before").unwrap();
        let mut host = rig.fifo();
        let mut sh = rig.spawn(ShellType::Zsh, zsh);
        let parked = sh.wait_for_after(&signed_p("AtermSpare=parked"), 0);
        tell(&mut host, "run");
        let spare = sh.wait_for_after(&signed_p("AtermStart=spare"), parked);
        assert!(
            sh.output()
                .contains(&signed_p("AtermStartFlags=pre:1,s:0,j:0,r:0")),
            "{ro} readonly: one hook ahead of aterm's\n{}",
            sh.transcript()
        );
        std::fs::write(rig.home.join("f"), "after").unwrap();
        tell(&mut host, "go");
        let a = sh.wait_for_after("\x1b]133;A", spare);
        let warm_prompt = sh.wait_for_after("P> ", a);
        assert_eq!(
            stdout_between(&sh, a, warm_prompt),
            cold,
            "{ro} readonly: the warm prompt's `$?` is the cold one's"
        );
        assert_eq!(
            std::fs::read_to_string(rig.home.join("seen")).unwrap(),
            "after",
            "{ro} readonly: the hook ahead of aterm's re-ran after the release"
        );
        assert_eq!(
            sh.stderr_text().matches("HOOKERR42").count()
                + sh.output().matches("HOOKERR42").count(),
            1,
            "{ro} readonly: the re-run is silenced\n{}",
            sh.transcript()
        );
        sh.finish();
    }

    for unwinding in [false, true] {
        let rig = SpareRig::new();
        rig.rc(
            ".zshrc",
            "sched +600 :\nsleep 600 &\necho $! >$HOME/job\nPERIOD=600\nperiodic() { :; }\n",
        );
        let mut host = rig.fifo();
        let mut sh = rig.spawn_with_job(ShellType::Zsh, zsh, &host);
        let parked = sh.wait_for_after(&signed_p("AtermSpare=parked"), 0);
        tell(&mut host, "run");
        sh.wait_for_after(&signed_p("AtermStart=spare"), parked);
        assert!(
            sh.output()
                .contains(&signed_p("AtermStartFlags=pre:0,s:1,j:1,r:1")),
            "{}",
            sh.transcript()
        );
        assert_job_cleanup(sh, unwinding);
    }
}

/// Observe the real job before normal cleanup or an assertion-like unwind.
/// Both must retire that same process and close both pumps; killing `script`
/// alone leaves the job-control process group and its pipe descriptors alive.
#[cfg(unix)]
fn assert_job_cleanup(sh: LiveShell, unwinding: bool) {
    let job = sh.job.as_ref().expect("armed before the waits");
    let owned = job.owned().expect("a current descendant job");
    let (stdout, stderr) = (sh.stdout.clone(), sh.stderr.clone());
    assert!(owned.current(), "the job really is running");
    if unwinding {
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let _shell = sh;
            panic!("injected assertion after the background job started");
        }));
        assert!(caught.is_err());
    } else {
        sh.kill_now();
    }
    assert!(
        owned.ended(),
        "the original job ended before its guard returned"
    );
    for pipe in [stdout, stderr] {
        assert!(pipe.eof.load(std::sync::atomic::Ordering::SeqCst));
    }
}

/// The history a shell holds and leaves, read back the same way for a cold tab
/// and a warm one: the in-memory list with its event numbers, `$HISTCMD`, the
/// prompt's `%!`, `$SAVEHIST`, then the file after `exit`. Timestamps are blanked.
#[cfg(unix)]
fn zsh_history_reading(rig: &SpareRig, warm: bool, append_during_hold: &str) -> String {
    let zsh = spare_zsh();
    let mut host = warm.then(|| rig.fifo());
    let mut sh = rig.spawn(ShellType::Zsh, zsh);
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
    sh.wait_for_line_editor();
    let from = sh.output().len();
    sh.send(
        "print -r -- \"DU\"\"MP $HISTCMD ${(%):-%!} $SAVEHIST\"; fc -l 1; print -r -- END$((40+2))",
    );
    let end = sh.wait_for_after("END42", from);
    let dump_from = sh.output()[..end].rfind("DUMP ").expect("dump");
    let dump = stdout_between(&sh, dump_from, end);
    sh.finish();
    let file = std::fs::read_to_string(rig.home.join("hist")).unwrap_or_default();
    let blank = |text: &str| -> String {
        text.lines()
            .map(|l| {
                // Blank extended-history timestamp.
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
/// tab's, for 8 history options set as the 16 words of the extended Hamming
/// code (a strength-3 covering array: every three options take each of their
/// eight on/off assignments in some rc, so a difference that any three options
/// make between a warm and a cold tab is met), plus a zsh with no append option
/// (it rewrites its file at exit) and SAVEHIST under HISTSIZE, and one whose rc
/// unsets SAVEHIST (at 0: zsh goes on saving at an unset SAVEHIST's last value,
/// which no parameter reads back), with the file
/// appended during the hold and left unchanged. The release reads the file
/// again as a cold start reads it (`fc -p`, the rc's sizes kept), with the
/// stale list pushed under at SAVEHIST 0, so it is never written back.
#[cfg(unix)]
#[test]
fn a_warm_zsh_holds_and_leaves_the_history_a_cold_one_does() {
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
    const GENERATORS: [u8; 4] = [0b0000_1111, 0b0011_0011, 0b0101_0101, 0b1111_1111];
    let words: Vec<u8> = (0..16u8)
        .map(|k| {
            (0..4)
                .filter(|i| k & (1 << i) != 0)
                .fold(0, |w, i| w ^ GENERATORS[i])
        })
        .collect();
    let mut combos: Vec<String> = words
        .iter()
        .map(|w| {
            (0..8)
                .filter(|i| w & (1 << i) != 0)
                .map(|i| format!("setopt {}\n", OPTS[i]))
                .collect()
        })
        .collect();
    combos.push("unsetopt append_history\nSAVEHIST=3\n".to_owned());
    combos.push("SAVEHIST=0\nunset SAVEHIST\n".to_owned());
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
                            "HISTFILE=\"$HOME/hist\"\nHISTSIZE=50\nSAVEHIST=50\n{opts}PS1='P> '\n"
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
        "{} of {} differ:\n{}",
        failures.len(),
        2 * combos.len(),
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
        sh.wait_for_after("\x1b]133;A", spare);
        sh.wait_for_line_editor();
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
    sh.wait_for_line_editor();
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

/// S4, bash: the same for four history knobs set as the eight masks of even
/// weight (each knob is the parity of the other three, so every three knobs
/// take all eight of their assignments, all four set and none among them), plus
/// a shell with HISTFILE unset, one under `set -e` whose HISTFILE is a
/// directory (bash's own load reads nothing and goes on; a release that read
/// anything but a regular file would end the shell), one whose rc saved a
/// line of its own (`history -s`: bash then reads no file at its start, and
/// the release reads none either), and one under `set -e` whose history
/// variables are readonly (a hardened /etc/profile: the release's assignments
/// fail alone, and its first prompt is drawn). bash 3.2 has no fork-free stat,
/// so the release always reads the list again (`history -c; history -r`),
/// which is what a cold start reads from the same file.
#[cfg(unix)]
#[test]
fn a_warm_bash_holds_and_leaves_the_history_a_cold_one_does() {
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
        .filter(|mask| mask.count_ones() % 2 == 0)
        .map(|mask| {
            let knobs: String = (0..4)
                .filter(|b| mask & (1 << b) != 0)
                .map(|b| KNOBS[b])
                .collect();
            format!("HISTFILE=\"$HOME/hist\"\nHISTSIZE=50\n{knobs}PS1='P> '\n")
        })
        .collect();
    cases.extend(
        [
            "unset HISTFILE\n",
            "mkdir \"$HOME/hd\"\nHISTFILE=\"$HOME/hd\"\nset -e\n",
            "HISTFILE=\"$HOME/hist\"\nHISTSIZE=50\nhistory -s 'echo rc'\n",
            "HISTFILE=\"$HOME/hist\"\nHISTSIZE=50\nHISTFILESIZE=50\n\
             readonly HISTFILE HISTSIZE HISTFILESIZE\nset -e\n",
        ]
        .map(|rc| format!("{rc}PS1='P> '\n")),
    );
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
        "{} of {} differ:\n{}",
        failures.len(),
        2 * cases.len(),
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

    let bash_3_2 = aterm_spec::host::tool("bash-3.2");
    let bash = aterm_spec::host::test_shell(bash_3_2.name)
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|| aterm_spec::host::could_not_run(bash_3_2.refusal()));
    let major = bash_major_version(&bash);
    if major >= 5 {
        aterm_spec::host::could_not_run(format!(
            "{bash} is bash {major}, which restarts the interrupted open, so the negative \
             control has no cut hold to show; {}",
            bash_3_2.refusal()
        ));
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
    rig.make_fifo();
    let mut sh = rig.spawn_from(ShellType::Bash, &bash, &cut);
    let parked = sh.wait_for_after(&signed_p("AtermSpare=parked"), 0);
    let mut host = rig.writer_met_by(&sh);
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
/// byte for byte what it was, a line another tab appended during the hold
/// included), for shells that would write at a hang-up: a zsh with no append
/// option rewrites its file from its list, and a bash whose rc saved a line
/// appends it. A shell whose FIFO is gone ends itself.
#[cfg(unix)]
#[test]
fn a_discarded_spare_writes_no_history_and_ends_without_its_fifo() {
    for (shell, program, _) in spare_shells() {
        let (rc_name, rc) = match shell {
            ShellType::Zsh => (
                ".zshrc",
                "HISTFILE=\"$HOME/hist\"\nHISTSIZE=50\nSAVEHIST=50\nunsetopt append_history\n",
            ),
            _ => (
                ".bashrc",
                "HISTFILE=\"$HOME/hist\"\nHISTSIZE=50\nshopt -s histappend\nhistory -s 'echo rc'\n",
            ),
        };
        for at_warm in [false, true] {
            let rig = SpareRig::new();
            rig.rc(rc_name, rc);
            std::fs::write(rig.home.join("hist"), "echo kept\n").unwrap();
            let mut host = rig.fifo();
            let mut sh = rig.spawn(shell, program);
            let parked = sh.wait_for_after(&signed_p("AtermSpare=parked"), 0);
            if at_warm {
                tell(&mut host, "run");
                sh.wait_for_after(&signed_p("AtermStart=spare"), parked);
            }
            {
                use std::io::Write as _;
                std::fs::OpenOptions::new()
                    .append(true)
                    .open(rig.home.join("hist"))
                    .and_then(|mut f| f.write_all(b"echo other\n"))
                    .expect("another tab's line");
            }
            let pid = sh.shell_pid();
            signal("HUP", pid);
            sh.wait_exit();
            assert_eq!(
                std::fs::read_to_string(rig.home.join("hist")).unwrap(),
                "echo kept\necho other\n",
                "{program}: a spare hung up at the {} hold writes no history",
                if at_warm { "warm" } else { "parked" }
            );
        }
        let rig = SpareRig::new();
        rig.rc(rc_name, rc);
        rig.make_fifo();
        let mut sh = rig.spawn(shell, program);
        sh.wait_for_after(&signed_p("AtermSpare=parked"), 0);
        let host = rig.writer_met_by(&sh);
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

/// A fish (the pty is aterm-pty's, in process): COULD NOT RUN where the host has none.
#[cfg(unix)]
fn spare_fish() -> &'static str {
    need_fish()
}

/// No live fish of this crate's makes its man-page completions. A first prompt
/// that finds the mark of their making missing starts fish_update_completions,
/// a detached python that outlives the test ([`mark_fish_completions_made`]),
/// and fish 3 and fish 4 alike ask `__fish_anypython` for that python first. A
/// conf.d snippet of the test's answers the ask: it notes it in the home and
/// says there is none, so no python starts. Two lanes each run a fish to its
/// first prompt and a command typed after it, which runs once that prompt's
/// handlers have: `spawn_live_fish`, whose cache and data dirs are the
/// hermetic home's, and the spare rig's, whose data dir is its own home's.
/// NEGATIVE CONTROL, in the test: the same snippet first removes the marks
/// where the shell's environment points fish (`$XDG_CACHE_HOME`,
/// `$XDG_DATA_HOME`), and that fish asks, so a fish that never asked could not
/// pass the test.
#[cfg(unix)]
#[test]
fn no_live_fish_starts_its_completion_generator() {
    const ANSWER: &str = "function __fish_anypython; touch $HOME/python-asked; return 1; end\n";
    const UNMARK: &str = "rmdir $XDG_CACHE_HOME/fish/generated_completions \
                          $XDG_DATA_HOME/fish/generated_completions\n";
    /// Whether `sh`, its home `home`, asked by the time a command typed at its
    /// first prompt ran.
    fn asked(mut sh: LiveShell, home: &Path) -> bool {
        let prompt = sh.wait_for_after("\x1b]133;A", 0);
        sh.send("echo RAN(math 40 + 2)");
        sh.wait_for_after("RAN42", prompt);
        let asked = home.join("python-asked").exists();
        sh.finish();
        asked
    }
    let fish = spare_fish();
    for unmarked in [false, true] {
        let snippet = if unmarked {
            format!("{UNMARK}{ANSWER}")
        } else {
            ANSWER.to_owned()
        };
        let fx = LiveFixture::new();
        let conf_d = fx.home.join("config").join("fish").join("conf.d");
        std::fs::create_dir_all(&conf_d).expect("conf.d");
        std::fs::write(conf_d.join("python.fish"), &snippet).expect("snippet");
        let hermetic = asked(spawn_live_fish(fish, &fx, &[]), &fx.home);
        let rig = SpareRig::new();
        rig.fish_rc("conf.d/python.fish", &snippet);
        let spare_rig = asked(rig.spawn(ShellType::Fish, fish), &rig.home);
        assert_eq!(
            (hermetic, spare_rig),
            (unmarked, unmarked),
            "fish asked for a python (hermetic lane, spare rig), its marks {}",
            if unmarked { "removed" } else { "kept" }
        );
    }
}

/// S1, fish: the WARM hold alone. fish reads no file of aterm's before its
/// startup files, so with the FIFO present its config runs at once, unseen,
/// then it signs `spare` and draws no prompt until `go`; what is typed
/// meanwhile (unechoed: fish has the tty in its own mode) runs after the
/// release. It never says it is parked, and signs
/// one start record. Negative control: with no FIFO the same fish signs
/// `cold` and draws its prompt at once.
#[cfg(unix)]
#[test]
fn a_warm_fish_runs_its_startup_files_at_once_and_holds_its_first_prompt() {
    let fish = spare_fish();
    let rc_body = "touch \"$HOME/rc.ran\"\n";
    let rig = SpareRig::new();
    rig.fish_rc("config.fish", rc_body);
    let mut sh = rig.spawn(ShellType::Fish, fish);
    let start = sh.wait_for_after(&signed_p("AtermStart=cold"), 0);
    sh.wait_for_after("\x1b]133;A", start);
    assert!(rig.ran("rc.ran"), "fish: a cold shell runs its config");
    sh.finish();

    let rig = SpareRig::new();
    rig.fish_rc("config.fish", rc_body);
    let mut host = rig.fifo();
    let mut sh = rig.spawn(ShellType::Fish, fish);
    let spare = sh.wait_for_after(&signed_p("AtermStart=spare"), 0);
    assert!(rig.ran("rc.ran"), "fish: a spare's config ran at once");
    sh.send("echo TYPED(math 40 + 2)");
    assert!(
        !sh.output().contains("\x1b]133;A"),
        "fish: no prompt while held\n{}",
        sh.transcript()
    );
    tell(&mut host, "go");
    let prompt = sh.wait_for_after("\x1b]133;A", spare);
    sh.wait_for_after("TYPED42", prompt);
    let out = sh.output();
    assert!(
        out.matches("AtermStart=").count() == 1 && !out.contains("AtermSpare="),
        "fish: one start record, and never parked\n{}",
        sh.transcript()
    );
    sh.finish();
}

/// One fish's first prompt as the test reads it: the `$status` its prompt
/// function was handed, the start record's flags, and what a prompt-event
/// handler defined ahead of aterm's (a conf.d snippet sourced first) read when
/// it last ran. Warm: held, `state` rewritten, then released.
#[cfg(unix)]
fn fish_first_prompt(fish: &str, warm: bool) -> (String, String, String) {
    let rig = SpareRig::new();
    rig.fish_rc(
        "config.fish",
        "function fish_prompt; printf 'ST=%s> ' $status; end\nfalse\n",
    );
    rig.fish_rc(
        "conf.d/00-ahead.fish",
        "function __ahead --on-event fish_prompt; cat $HOME/state >$HOME/seen; end\n",
    );
    rig.rc("state", if warm { "before\n" } else { "after\n" });
    let mut host = warm.then(|| rig.fifo());
    let mut sh = rig.spawn(ShellType::Fish, fish);
    let start = sh.wait_for_after(
        &signed_p(&format!(
            "AtermStart={}",
            if warm { "spare" } else { "cold" }
        )),
        0,
    );
    if let Some(host) = host.as_mut() {
        rig.rc("state", "after\n");
        tell(host, "go");
    }
    let prompt = sh.wait_for_after("ST=", start);
    let status_end = sh.wait_for_after("> ", prompt);
    let status = sh
        .value_before("ST=", status_end)
        .trim_end_matches("> ")
        .to_owned();
    let flags = stdout_between(&sh, 0, start)
        .rsplit("AtermStartFlags=")
        .next()
        .and_then(|f| f.split(';').next())
        .unwrap_or_default()
        .to_owned();
    sh.send("echo SEEN(math 40 + 2)=(cat $HOME/seen)");
    let end = sh.wait_for_after("SEEN42=", prompt);
    let seen_end = sh.wait_for_after("\n", end);
    let seen = sh.value_before("SEEN42=", seen_end);
    sh.finish();
    (status, flags, seen)
}

/// S2, fish: a warm tab's first prompt reads as a cold tab's. Its prompt is
/// handed the `$status` a cold one's is (a config ending in `false`), and a
/// prompt-event handler defined ahead of aterm's (`pre:1` in the flags) runs
/// again at the release, so what it read is what the file held then, not at
/// the spawn. The cold shell, started into what the warm one is released
/// into, is the reading the warm one must give; without the re-run the warm
/// one read `before` (the negative control, run by hand).
#[cfg(unix)]
#[test]
fn a_warm_fish_first_prompt_reads_as_a_cold_one() {
    let fish = spare_fish();
    let cold = fish_first_prompt(fish, false);
    let warm = fish_first_prompt(fish, true);
    assert_eq!(
        cold.1, "pre:1,s:0,j:0,r:0",
        "fish: the flags name the one handler ahead of aterm's"
    );
    assert_eq!(warm, cold, "fish: (status, flags, the handler's reading)");

    for unwinding in [false, true] {
        let rig = SpareRig::new();
        rig.fish_rc("config.fish", "sleep 600 &\necho $last_pid >$HOME/job\n");
        let host = rig.fifo();
        let mut sh = rig.spawn_with_job(ShellType::Fish, fish, &host);
        sh.wait_for_after(&signed_p("AtermStart=spare"), 0);
        assert!(
            sh.output()
                .contains(&signed_p("AtermStartFlags=pre:0,s:0,j:1,r:0")),
            "fish: the job is counted\n{}",
            sh.transcript()
        );
        assert_job_cleanup(sh, unwinding);
    }
}

/// The prompt-event handlers a fish start record counts as ahead of aterm's
/// (`pre:`), and a warm spare runs again at its release: the user's, defined
/// before the integration loaded, in their order — not one named `__aterm_*`,
/// not one defined after aterm's — and none where aterm's is the first left
/// (fish's own first-prompt handler and aterm's two erase themselves before
/// aterm's runs, so the test erases them as that prompt would have).
#[cfg(unix)]
#[test]
fn a_fish_counts_the_prompt_handlers_defined_ahead_of_aterms_alone() {
    let fish = need_fish();
    let script = format!(
        "{}/src/scripts/aterm_shell_integration.fish",
        env!("CARGO_MANIFEST_DIR")
    );
    let output = shell_command(fish)
        .arg("-i")
        .arg("-c")
        .arg(
            "functions -e __fish_on_interactive; \
             function __user_a --on-event fish_prompt; end; \
             function __aterm_mine --on-event fish_prompt; end; \
             function __user_b --on-event fish_prompt; end; \
             source \"$ATERM_TEST_SCRIPT\" >/dev/null 2>&1; \
             function __user_after --on-event fish_prompt; end; \
             functions -e __aterm_reroute_first_prompt __aterm_prompt_wrap_first; \
             printf 'AHEAD=%s;' (__aterm_prompt_handlers_ahead); \
             functions -e __user_a __aterm_mine __user_b; \
             printf 'NONE=%s;' (__aterm_prompt_handlers_ahead)",
        )
        .env("ATERM_TEST_SCRIPT", &script)
        .output()
        .expect("spawn fish");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "AHEAD=__user_a;AHEAD=__user_b;NONE=;",
        "fish: the handlers ahead of aterm's; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// A fish start record names the config folder as 633;E escapes a command
/// line: a folder with nothing to escape as it is, and one holding any one of
/// the reserved bytes (a space, `;`, a backslash, a tab) escaped. It is written
/// to the shell's stdout, where every other mark of fish's goes (not /dev/tty,
/// which this test's fish has none of).
#[cfg(unix)]
#[test]
fn a_fish_start_record_names_its_config_folder_escaped_on_stdout() {
    let fish = need_fish();
    let root = aterm_tempfile::tempdir().expect("tempdir");
    for name in ["plain-1.~", "my cfg", "a;b", "back\\slash", "tab\there"] {
        let config = root.path().join(name);
        std::fs::create_dir_all(config.join("fish")).expect("config dir");
        let folder = format!("{}/fish", config.to_str().expect("a UTF-8 temp path"));
        let rc = String::from_utf8(osc633_encode(&folder)).expect("UTF-8 in, UTF-8 out");
        assert_eq!(
            run_fish_integration(
                fish,
                &[(
                    "XDG_CONFIG_HOME",
                    config.to_str().expect("a UTF-8 temp path")
                )],
                "functions -e __fish_on_interactive; set -e __aterm_start_roots; \
                 __aterm_start_mark cold",
            ),
            format!(
                "\x1b]633;P;AtermStartRc={rc}\x07\x1b]633;P;AtermStartFlags=pre:0,s:0,j:0,r:0\x07\
                 \x1b]633;P;AtermStart=cold\x07"
            ),
            "fish: the start record for {folder:?}"
        );
    }
}

/// Negative control: the script before the record named its roots wrote none.
#[cfg(unix)]
#[test]
fn a_fish_start_record_names_the_roots_its_conf_d_loop_walks() {
    let fish = need_fish();
    for name in ["x d;s", "tab\there"] {
        let root = aterm_tempfile::tempdir().expect("tempdir");
        let inherited = root.path().join(name);
        let vendor = inherited.join("fish").join("vendor_conf.d");
        std::fs::create_dir_all(&vendor).expect("vendor conf.d");
        std::fs::copy(
            format!(
                "{}/src/scripts/aterm_shell_integration.fish",
                env!("CARGO_MANIFEST_DIR")
            ),
            vendor.join("aterm_shell_integration.fish"),
        )
        .expect("the script in a vendor conf.d");
        let mut cmd = shell_command(fish);
        let home = cmd.home().to_path_buf();
        std::fs::write(
            home.join(".config/fish/conf.d/00-probe.fish"),
            "string join \\n -- \"$__fish_sysconf_dir\" $__fish_vendor_confdirs $vendor_confdirs \
             >$HOME/walked\n",
        )
        .expect("the probe");
        let output = cmd
            .arg("-i")
            .arg("-c")
            .arg("functions -e __fish_on_interactive; __aterm_start_mark cold")
            .env(
                "XDG_DATA_DIRS",
                format!("{}:{}", inherited.display(), hermetic_data(&home).display()),
            )
            .output()
            .expect("spawn fish");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let walked = std::fs::read_to_string(home.join("walked")).expect("the loop's own list");
        let walked: Vec<&str> = walked.lines().collect();
        assert!(
            walked.contains(&vendor.to_str().expect("a UTF-8 temp path")),
            "fish: the inherited folder is walked: {walked:?}"
        );
        let encoded = |folder: &&str| {
            let one = String::from_utf8(osc633_encode(folder)).expect("UTF-8 in, UTF-8 out");
            if name.contains('\t') {
                one
            } else {
                one.replace("\\x20", " ")
            }
        };
        let roots = walked.iter().map(encoded).collect::<Vec<_>>().join("\\x00");
        assert!(
            stdout.contains(&format!(
                "\x1b]633;P;AtermStartRoots={roots}\x07\x1b]633;P;AtermStartRc="
            )),
            "fish: the roots its loop walked, {walked:?}, ahead of the rc folder\n{stdout:?}\n\
             stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

/// fish's clock is not the test's. fish 3 stamps and merges by libc's `time()`,
/// which glibc serves from `CLOCK_REALTIME_COARSE`, up to a tick behind the
/// wall clock (`CLOCK_REALTIME`, fish 4's and [`unix_secs`]'s). Measured in the
/// Linux lane's guest (fish 3.7.0, glibc 2.39), at the wall clock's turn of a
/// second `time()` still read the old one 8 of 8 times, for 0.7-1.7 ms: a
/// release sent at the turn merged a second early and missed the appends. And
/// the test's own `time()` is no stand-in for fish's: an x86_64 test under
/// Rosetta read the new second at the turn 8 of 8 times while the guest's
/// coarse clock, fish's, still read the old one. So the release waits until the
/// wall clock reads the second after the stamps': a clock fish reads trails it
/// by a tick, far less than that second, so by then each has reached them.
#[cfg(unix)]
fn fish_history_reading(
    fish: &str,
    warm: bool,
    rc_reads: bool,
    appended: bool,
    named: bool,
) -> String {
    let rig = SpareRig::new();
    let name = if named { "aterm_s3" } else { "fish" };
    let mut config = String::new();
    if named {
        config.push_str("set -g fish_history aterm_s3\n");
    }
    if rc_reads {
        config.push_str("history search --max 1 >/dev/null\n");
    }
    rig.fish_rc("config.fish", &config);
    let data = rig.home.join(".local").join("share").join("fish");
    std::fs::create_dir_all(&data).expect("fish data dir");
    let file = data.join(format!("{name}_history"));
    let entry = |cmd: &str, when: u64| format!("- cmd: {cmd}\n  when: {when}\n");
    std::fs::write(
        &file,
        format!(
            "{}{}",
            entry("echo one", 1_700_000_001),
            entry("echo two", 1_700_000_002)
        ),
    )
    .expect("history");
    let append = |when: u64| {
        use std::io::Write as _;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&file)
            .expect("history");
        f.write_all(
            format!("{}{}", entry("echo three", when), entry("echo four", when)).as_bytes(),
        )
        .expect("append");
    };
    let mut host = warm.then(|| rig.fifo());
    if appended && !warm {
        append(1_700_000_003);
    }
    let mut sh = rig.spawn(ShellType::Fish, fish);
    let kind = if warm { "spare" } else { "cold" };
    let start = sh.wait_for_after(&signed_p(&format!("AtermStart={kind}")), 0);
    if let Some(host) = host.as_mut() {
        if appended {
            let after_the_start = unix_secs() + 1;
            append(after_the_start);
            until_the_clock_reads(after_the_start + 1);
        }
        tell(host, "go");
    }
    let prompt = sh.wait_for_after("\x1b]133;A", start);
    sh.send(" echo HIST(math 40 + 2)=(history | string join ,)");
    let at = sh.wait_for_after("HIST42=", prompt);
    let reading_end = sh.wait_for_after("\n", at);
    let reading = sh.value_before("HIST42=", reading_end);
    sh.finish();
    reading
}

/// Whole seconds since the epoch by the wall clock, which leads every clock
/// fish stamps its history with.
#[cfg(unix)]
fn unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("a clock past 1970")
        .as_secs()
}

/// Return once the wall clock reads second `at` or later: a wait for the
/// clock, which must happen (within two seconds here), never for a process.
#[cfg(unix)]
fn until_the_clock_reads(at: u64) {
    let at = std::time::UNIX_EPOCH + std::time::Duration::from_secs(at);
    while let Ok(left) = at.duration_since(std::time::SystemTime::now()) {
        std::thread::sleep(left);
    }
}

/// S3, fish: the history a warm tab holds after its release is a cold tab's,
/// over the matrix of a config that reads the history at its start or not,
/// commands another session adds during the hold or not, and the default
/// history session or one the config names: the release takes in what
/// arrived (`history merge`), a history nothing read yet is read afresh at
/// first use, as a cold start reads it. The cold reading is the control each
/// warm one must match. A merge made before the hold began, or none, misses
/// the commands added during it ([`fish_history_reading`]'s stamps). The eight
/// cases share nothing and run side by side.
#[cfg(unix)]
#[test]
fn a_warm_fish_holds_the_history_a_cold_one_does() {
    let fish = spare_fish();
    let mut cases: Vec<(String, Box<dyn FnOnce() + Send>)> = Vec::new();
    for rc_reads in [false, true] {
        for appended in [false, true] {
            for named in [false, true] {
                let case = move || {
                    let cold = fish_history_reading(fish, false, rc_reads, appended, named);
                    let warm = fish_history_reading(fish, true, rc_reads, appended, named);
                    assert!(
                        cold.contains("echo two"),
                        "fish: the control read the file ({cold:?})"
                    );
                    assert_eq!(
                        warm, cold,
                        "fish: rc_reads={rc_reads} appended={appended} named={named}"
                    );
                };
                cases.push((
                    format!(
                        "fish-history-r{}-a{}-n{}",
                        u8::from(rc_reads),
                        u8::from(appended),
                        u8::from(named)
                    ),
                    Box::new(case),
                ));
            }
        }
    }
    side_by_side(cases);
}
