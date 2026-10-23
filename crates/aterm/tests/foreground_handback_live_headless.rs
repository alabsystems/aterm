// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The foreground handback, live under a real shell's job control.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::Path;
use std::time::Duration;

use aterm_spec::wait::MUST_HAPPEN;

use crate::headless_boot::{self, Instance};
use crate::live_ctl::Ctl;
use crate::standin;

const ENTRY: &str = "foreground_handback_live_headless::zsh_hands_a_killed_holders_modes_back_once";

const ZSH: &[&str] = &["/bin/zsh", "-f", "-i"];
const BASH: &[&str] = &["/bin/bash", "--norc", "--noprofile", "-i"];

const HANDED_BACK: &[(&str, &str)] = &[
    ("alt_screen", "false"),
    ("cursor_visible", "true"),
    ("mouse_mode", "none"),
    ("mouse_encoding", "x10"),
    ("kitty_keyboard", "none"),
    ("modify_other_keys", "0"),
    ("focus_reporting", "false"),
    ("sync_output", "false"),
];

const INCIDENT_MODES: &[u8] =
    b"\x1b[?1049h\x1b[?1003h\x1b[?1006h\x1b[>5u\x1b[>4;2m\x1b[?2004h\x1b[?1004h\x1b[?25l\x1b[?2026h";

/// The modes an [`Job::Armed`] job arms before it prints its marker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Arm {
    /// Any-motion mouse reporting (`?1003h`).
    Mouse,
    /// The alternate screen and any-motion mouse reporting.
    AltMouse,
}

impl Arm {
    fn bytes(self) -> &'static [u8] {
        match self {
            Arm::Mouse => b"\x1b[?1003h",
            Arm::AltMouse => b"\x1b[?1049h\x1b[?1003h",
        }
    }

    fn word(self) -> &'static str {
        match self {
            Arm::Mouse => "mouse",
            Arm::AltMouse => "alt-mouse",
        }
    }
}

/// The programs this lane types into its shells.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Job {
    /// A killed Claude Code: arm [`INCIDENT_MODES`], be answered, die by SIGKILL.
    Incident,
    /// Arm the modes, print `armed-<n>`, and live until a signal ends it
    /// (30 s at most).
    Armed(Arm, u32),
    /// Set mouse tracking (an input mode), be answered, print `answered`,
    /// exit 0.
    MouseOneShot,
    /// Run `vim -u NONE -c q` (the vim the test resolved, which it exports as
    /// `ATERM_TEST_VIM`), be answered, print `answered`, exit 0.
    Vim,
    /// gdb's shape: arm the alternate screen, mouse and a hidden cursor, fork
    /// a child into its own group and hand it the terminal, take the terminal
    /// back once the child exits (`parent-back`), then die by SIGKILL once
    /// the lane releases it with a line on its stdin.
    Holder,
}

impl Job {
    fn spec(self) -> String {
        match self {
            Job::Incident => "incident".into(),
            Job::Armed(arm, n) => format!("job:{}:{n}", arm.word()),
            Job::MouseOneShot => "one-shot".into(),
            Job::Vim => "vim".into(),
            Job::Holder => "holder".into(),
        }
    }

    fn parse(spec: &str) -> Option<Self> {
        Some(match spec {
            "incident" => Job::Incident,
            "one-shot" => Job::MouseOneShot,
            "vim" => Job::Vim,
            "holder" => Job::Holder,
            _ => {
                let rest = spec.strip_prefix("job:")?;
                let (arm, n) = rest.split_once(':')?;
                let arm = [Arm::Mouse, Arm::AltMouse]
                    .into_iter()
                    .find(|a| a.word() == arm)?;
                Job::Armed(arm, n.parse().ok()?)
            }
        })
    }

    fn line(self) -> String {
        standin::shell_line(ENTRY, &self.spec())
    }

    fn play(self) -> ! {
        match self {
            Job::Incident => {
                standin::put(INCIDENT_MODES);
                standin::answered();
                kill_self()
            }
            Job::Armed(arm, n) => {
                // SAFETY: the default disposition, then this thread's mask: a SIGINT sent to the
                // job's group reaches this thread whatever the inherited mask of the others.
                unsafe {
                    libc::signal(libc::SIGINT, libc::SIG_DFL);
                    let mut set: libc::sigset_t = std::mem::zeroed();
                    libc::sigemptyset(&mut set);
                    libc::sigaddset(&mut set, libc::SIGINT);
                    libc::pthread_sigmask(libc::SIG_UNBLOCK, &set, std::ptr::null_mut());
                }
                standin::put(arm.bytes());
                standin::put(format!("armed-{n}\n").as_bytes());
                std::thread::sleep(Duration::from_secs(30));
                standin::exit(0)
            }
            Job::MouseOneShot => {
                standin::put(b"\x1b[?1000h");
                standin::answered();
                standin::put(b"answered\n");
                standin::exit(0)
            }
            Job::Vim => {
                // Checked: a vim that did not run is never answered, so the lane cannot pass having
                // run no vim (docs/DESIGN-init-system-2026-10-20.md §8, rule 5).
                let ran = aterm_spec::host::find_tool("vim").map(|vim| {
                    #[expect(
                        clippy::disallowed_types,
                        reason = "test code: the stand-in job runs vim as a shell's job would"
                    )]
                    std::process::Command::new(vim)
                        .args(["-u", "NONE", "-c", "q"])
                        .status()
                });
                if !matches!(&ran, Some(Ok(status)) if status.success()) {
                    standin::put(format!("vim did not run: {ran:?}\n").as_bytes());
                    standin::exit(1)
                }
                standin::answered();
                standin::put(b"answered\n");
                standin::exit(0)
            }
            Job::Holder => holder(),
        }
    }
}

fn kill_self() -> ! {
    // SAFETY: a signal to this process; SIGKILL cannot be caught.
    unsafe {
        libc::kill(libc::getpid(), libc::SIGKILL);
    }
    loop {
        // SAFETY: `pause` blocks this thread until a signal arrives; the SIGKILL sent above ends
        // the process there.
        unsafe {
            libc::pause();
        }
    }
}

fn holder() -> ! {
    // SAFETY: SIGTTOU ignored, so the holder may take the terminal back from its child's group.
    unsafe {
        libc::signal(libc::SIGTTOU, libc::SIG_IGN);
    }
    standin::put(b"\x1b[?1049h\x1b[?1003h\x1b[?1006h\x1b[?25l");
    standin::answered();
    // SAFETY: the child calls only async-signal-safe functions and `_exit`s.
    let pid = unsafe { libc::fork() };
    if pid == 0 {
        // SAFETY: async-signal-safe calls on this process's own terminal.
        unsafe {
            libc::setpgid(0, 0);
            libc::tcsetpgrp(0, libc::getpgrp());
            let line = b"child-output\r\n";
            libc::write(1, line.as_ptr().cast(), line.len());
            standin::answered();
            libc::_exit(0);
        }
    }
    assert!(pid > 0, "fork: {}", std::io::Error::last_os_error());
    // SAFETY: reap the child forked above, then take the terminal back.
    unsafe {
        let mut status = 0;
        libc::waitpid(pid, &mut status, 0);
        libc::tcsetpgrp(0, libc::getpgrp());
    }
    standin::put(b"parent-back\r\n");
    let mut fd = libc::pollfd {
        fd: 0,
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: one valid `pollfd`, then a read into a live buffer.
    unsafe {
        if libc::poll(&mut fd, 1, 120_000) > 0 {
            let mut buf = [0u8; 256];
            libc::read(0, buf.as_mut_ptr().cast(), buf.len());
        }
    }
    kill_self()
}

fn play_standin_if_asked() {
    let Some(spec) = standin::spec() else { return };
    match Job::parse(&spec) {
        Some(job) => job.play(),
        None => {
            standin::put(format!("unknown job: {spec}\r\n").as_bytes());
            standin::exit(2)
        }
    }
}

/// Who should hold the terminal for the next key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Holder {
    /// The shell's own group.
    Shell,
    /// Any other group: a job.
    Job,
}

struct Shell {
    inst: Instance,
    ctl: Ctl,
    /// The shell's process group: the terminal's foreground group from its
    /// spawn until it runs a job.
    pgrp: i32,
    settles: u32,
}

impl Shell {
    /// Boot an instance whose only session is `argv`; an environment refusal
    /// is COULD NOT RUN, never a pass ([`headless_boot::boot_with`]).
    #[track_caller]
    fn boot(tag: &str, argv: &[&str]) -> Self {
        let inst = headless_boot::boot_with(
            &format!("atfh{tag}"),
            |_, cmd| {
                cmd.arg("-e").args(argv);
            },
            |_| true,
        );
        let mut ctl = Ctl::connect(&inst.sock);
        let pgrp = aterm_spec::wait::until_within(
            MUST_HAPPEN,
            "the instance to name its terminal's foreground group (`who` fgpgid=)",
            || ctl.fg_pgrp(),
        );
        Self {
            inst,
            ctl,
            pgrp,
            settles: 0,
        }
    }

    /// Wait (a hang detector) until `who` names `want` as the terminal's
    /// holder; `what` names the step that waits.
    #[track_caller]
    fn await_holder(&mut self, want: Holder, what: &str) {
        let shell = self.pgrp;
        let mut last = None;
        let held = aterm_spec::wait::within(MUST_HAPPEN, Duration::from_millis(10), || {
            last = self.ctl.fg_pgrp();
            match (want, last) {
                (Holder::Shell, Some(g)) if g == shell => Some(()),
                (Holder::Job, Some(g)) if g != shell => Some(()),
                _ => None,
            }
        });
        assert!(
            held.is_some(),
            "{what}: the {want:?} never held the terminal (fgpgid={last:?}, shell={shell}); \
             processes:\n{}",
            headless_boot::process_tree(&self.inst)
        );
    }

    /// Type `line` into the shell and press Enter, once the shell holds the
    /// terminal (a job holding it would read the line as its own input).
    #[track_caller]
    fn type_line(&mut self, line: &str) {
        self.await_holder(Holder::Shell, &format!("before typing {line:?}"));
        self.ctl.type_line(line);
    }

    /// Start `job` from the shell.
    #[track_caller]
    fn start(&mut self, job: Job) {
        self.type_line(&job.line());
    }

    /// Ctrl+`letter` to the job, once a job holds the terminal.
    #[track_caller]
    fn job_ctrl(&mut self, letter: char, what: &str) {
        self.await_holder(Holder::Job, what);
        self.ctl.ok(&format!("ctrl {letter}"));
    }

    /// Return once the shell has run everything typed so far: a builtin (no
    /// fork, so no foreground edge) prints a fresh marker, and the shell reads
    /// that line only after the command before it is over, so every byte that
    /// command wrote, and the edge at its end, is parsed by then.
    #[track_caller]
    fn settle(&mut self) {
        self.settles += 1;
        let n = self.settles;
        self.type_line(&format!("printf '%s\\n' settled\"\"-{n}"));
        self.ctl.expect_on_screen(&format!("settled-{n}"));
    }

    fn handbacks(&mut self) -> Vec<String> {
        self.ctl
            .counted("timeline 500")
            .1
            .into_iter()
            .filter(|l| l.split_whitespace().any(|t| t == "kind=modes-restored"))
            .collect()
    }

    fn handback_count(&mut self) -> usize {
        self.handbacks().len()
    }

    fn modes_of(&mut self, want: &[(&str, &str)]) -> (BTreeMap<String, String>, bool) {
        let modes = self.ctl.modes();
        let all = want
            .iter()
            .all(|(k, v)| modes.get(*k).map(String::as_str) == Some(*v));
        (modes, all)
    }

    /// Every `(mode, value)` in `want` holds now.
    #[track_caller]
    fn assert_modes(&mut self, what: &str, want: &[(&str, &str)]) {
        let (modes, all) = self.modes_of(want);
        assert!(all, "{what}: want {want:?}, got {modes:?}");
    }

    /// Wait (a hang detector) for every `(mode, value)` in `want`, for a mode
    /// that lands after the notice a test waited on (zle re-arms bracketed
    /// paste only when it draws its next prompt), then assert it.
    #[track_caller]
    fn await_modes(&mut self, what: &str, want: &[(&str, &str)]) {
        let _ = aterm_spec::wait::within(MUST_HAPPEN, Duration::from_millis(20), || {
            let (modes, all) = self.modes_of(want);
            let unnamed = want.iter().any(|(k, _)| !modes.contains_key(*k));
            (all || unnamed).then_some(())
        });
        self.assert_modes(what, want);
    }

    /// The session holds `want` handbacks, and none of them reverted 1007:
    /// alternate scroll is on by default and no job here touches it, so a
    /// handback that names it reverted a mode nobody armed (`modes` does not
    /// print 1007; the event's `reverted=` does).
    #[track_caller]
    fn assert_handbacks(&mut self, what: &str, want: usize) {
        let events = self.handbacks();
        assert_eq!(
            events.len(),
            want,
            "{what}: modes-restored events:\n{}",
            events.join("\n")
        );
        assert!(
            events.iter().all(|e| !e.contains("alternate-scroll")),
            "{what}: a handback reverted alternate-scroll:\n{}",
            events.join("\n")
        );
    }

    fn signal_instance(&self, sig: libc::c_int) {
        let pid = libc::pid_t::try_from(self.inst.child.id()).expect("a pid");
        // SAFETY: a signal to the instance this test spawned and has not reaped.
        assert_eq!(unsafe { libc::kill(pid, sig) }, 0, "signal the instance");
    }
}

fn handed_back(bracketed_paste: &'static str) -> Vec<(&'static str, &'static str)> {
    let mut want = HANDED_BACK.to_vec();
    want.push(("bracketed_paste", bracketed_paste));
    want
}

fn held_fifo(path: &Path) -> std::fs::File {
    let c = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).expect("a C path");
    // SAFETY: a valid NUL-terminated path.
    assert_eq!(
        unsafe { libc::mkfifo(c.as_ptr(), 0o600) },
        0,
        "mkfifo {}: {}",
        path.display(),
        std::io::Error::last_os_error()
    );
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .expect("hold the FIFO read-write")
}

#[test]
fn zsh_hands_a_killed_holders_modes_back_once() {
    play_standin_if_asked();
    let mut sh = Shell::boot("zi", ZSH);
    sh.start(Job::Incident);
    sh.ctl.expect_on_screen("zsh:.killed");
    sh.await_modes("the incident is handed back", &handed_back("true"));
    sh.assert_handbacks("exactly one handback", 1);

    sh.start(Job::MouseOneShot);
    sh.ctl.expect_on_screen("answered");
    sh.settle();
    sh.assert_modes(
        "a one-shot after the incident is handed back",
        &[("mouse_mode", "none")],
    );
    sh.assert_handbacks("the next job's exit is the next handback", 2);
}

#[test]
fn zsh_keeps_the_display_modes_a_one_shot_sets() {
    let mut sh = Shell::boot("zd", ZSH);
    sh.type_line("tput rmam");
    sh.settle();
    sh.assert_modes("tput rmam is kept", &[("auto_wrap", "false")]);
    sh.type_line("tput smam");
    sh.type_line("tput civis");
    sh.settle();
    sh.assert_modes("tput civis is kept", &[("cursor_visible", "false")]);
    sh.type_line("tput cnorm");
    sh.assert_handbacks("tput rmam and civis record nothing", 0);

    let release = sh.inst.tmp.join("wrapped.release");
    let mut fifo = held_fifo(&release);
    sh.type_line(&format!(
        "tput smcup; sh -c 'echo in\"\"side; read line' <'{}'",
        release.display()
    ));
    sh.ctl.expect_on_screen("inside");
    sh.assert_modes(
        "tput smcup; cmd runs cmd on the alt screen",
        &[("alt_screen", "true")],
    );
    fifo.write_all(b"go\n")
        .expect("release the wrapped command");
    sh.settle();
    sh.assert_modes(
        "the alt screen outlives the wrapped command",
        &[("alt_screen", "true")],
    );
    sh.type_line("tput rmcup");
    sh.settle();
    sh.assert_modes("tput rmcup leaves it", &[("alt_screen", "false")]);
    sh.assert_handbacks("the whole wrapper records nothing", 0);
}

#[test]
fn zsh_records_nothing_for_a_clean_vim_exit() {
    let vim = aterm_spec::host::need_tool("vim");
    let vim = format!("'{}'", vim.display().to_string().replace('\'', r"'\''"));
    let mut sh = Shell::boot("zv", ZSH);
    sh.type_line(&format!("export ATERM_TEST_VIM={vim}"));
    sh.start(Job::Vim);
    sh.ctl.expect_on_screen("answered");
    sh.settle();
    sh.assert_handbacks("a clean vim exit records nothing", 0);
    sh.type_line(&format!("{vim} -u NONE -c q && echo vim\"\"-done"));
    sh.ctl.expect_on_screen("vim-done");
    let bare = sh.handback_count();
    assert!(bare <= 1, "a bare vim exit records at most one: {bare}");
}

#[test]
fn zsh_a_stopped_job_keeps_its_modes_until_it_dies() {
    let mut sh = Shell::boot("zs", ZSH);
    sh.start(Job::Armed(Arm::Mouse, 1));
    sh.ctl.expect_on_screen("armed-1");
    sh.job_ctrl('z', "the mouse job holds the terminal for its Ctrl-Z");
    sh.ctl.expect_on_screen("suspended");
    sh.assert_modes(
        "Ctrl-Z keeps a stopped job's mouse",
        &[("mouse_mode", "any")],
    );
    sh.assert_handbacks("the stop edge hands nothing back", 0);
    sh.type_line("fg");
    sh.await_holder(Holder::Job, "fg gives the stopped job the terminal");
    sh.assert_modes(
        "fg resumes the job with its mouse",
        &[("mouse_mode", "any")],
    );
    sh.assert_handbacks("fg hands nothing back", 0);
    sh.job_ctrl('c', "the resumed job holds the terminal for its Ctrl-C");
    sh.settle();
    sh.assert_modes(
        "the job's death hands its mouse back",
        &[("mouse_mode", "none")],
    );
    sh.assert_handbacks("the death edge is one handback", 1);
}

#[test]
fn zsh_a_bgd_leaderless_pipeline_keeps_its_modes_beside_a_command() {
    let mut sh = Shell::boot("zb", ZSH);
    let both = [("alt_screen", "true"), ("mouse_mode", "any")];
    sh.type_line(&format!(
        "sleep 0 | {}",
        Job::Armed(Arm::AltMouse, 3).line()
    ));
    sh.ctl.expect_on_screen("armed-3");
    let shell = sh.pgrp;
    aterm_spec::wait::until_within(MUST_HAPPEN, "the pipeline's leader to exit", || {
        let group = sh.ctl.fg_pgrp().filter(|g| *g > 0 && *g != shell)?;
        // SAFETY: signal 0 sends nothing; `kill` only says whether `group`'s leader lives.
        let gone = unsafe { libc::kill(group, 0) } == -1
            && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
        gone.then_some(())
    });
    sh.job_ctrl('z', "the pipeline to bg holds the terminal for its Ctrl-Z");
    sh.ctl.expect_on_screen("suspended");
    sh.type_line("bg");
    sh.type_line("/bin/echo bystander-edge");
    sh.settle();
    sh.assert_modes(
        "a command beside a bg'd leaderless pipeline keeps its modes",
        &both,
    );
    sh.assert_handbacks("the bystander edges hand nothing back", 0);
    sh.type_line("fg");
    sh.await_holder(Holder::Job, "fg gives the bg'd pipeline the terminal");
    sh.assert_modes("fg resumes the bg'd pipeline with its modes", &both);
    sh.job_ctrl('c', "the bg'd pipeline holds the terminal for its Ctrl-C");
    sh.settle();
    sh.assert_modes(
        "the bg'd pipeline's death hands back",
        &[("alt_screen", "false"), ("mouse_mode", "none")],
    );
    sh.assert_handbacks("the bg'd pipeline's death is one handback", 1);
}

#[test]
fn zsh_a_live_holder_keeps_its_modes_across_its_child() {
    let mut sh = Shell::boot("zh", ZSH);
    sh.start(Job::Holder);
    sh.ctl.expect_on_screen("parent-back");
    sh.assert_modes(
        "a live holder keeps its modes across its child",
        &[
            ("alt_screen", "true"),
            ("mouse_mode", "any"),
            ("cursor_visible", "false"),
        ],
    );
    sh.assert_handbacks("the child's exit hands nothing back", 0);
    sh.await_holder(
        Holder::Job,
        "the live holder holds the terminal for its release",
    );
    sh.ctl.ok("key enter");
    sh.ctl.expect_on_screen("zsh:.killed");
    sh.await_modes("the holder's death hands back", HANDED_BACK);
    sh.assert_handbacks("the holder's death is one handback", 1);
}

#[test]
fn zsh_a_missed_one_shot_loses_only_its_own_handback() {
    let mut sh = Shell::boot("zm", ZSH);
    let started = sh.inst.tmp.join("missed.started");
    let gate = sh.inst.tmp.join("missed.gate");
    let flag = sh.inst.tmp.join("missed.flag");
    let mut fifo = held_fifo(&gate);
    sh.type_line(&format!(
        ": >'{}'; read -r go <'{}'; /usr/bin/printf '\\033[?1000h'; echo miss\"\"ed-done; : >'{}'",
        started.display(),
        gate.display(),
        flag.display()
    ));
    aterm_spec::wait::until_within(
        MUST_HAPPEN,
        "the shell to start the missed one-shot's line",
        || started.exists().then_some(()),
    );
    sh.signal_instance(libc::SIGSTOP);
    fifo.write_all(b"go\n").expect("open the one-shot's gate");
    let shell_done = aterm_spec::wait::within(MUST_HAPPEN, Duration::from_millis(10), || {
        flag.exists().then_some(())
    });
    sh.signal_instance(libc::SIGCONT);
    assert!(
        shell_done.is_some(),
        "the shell never ran the missed one-shot's line behind the stopped instance"
    );
    sh.ctl.expect_on_screen("missed-done");
    sh.settle();
    sh.assert_modes(
        "the missed one-shot's mouse stays (the residual)",
        &[("mouse_mode", "normal")],
    );
    sh.assert_handbacks("the missed one-shot loses its handback", 0);

    sh.start(Job::Armed(Arm::Mouse, 4));
    sh.ctl.expect_on_screen("armed-4");
    sh.job_ctrl('c', "the re-arming job holds the terminal for its Ctrl-C");
    sh.settle();
    sh.assert_modes(
        "a job re-arming the missed mouse hands it back",
        &[("mouse_mode", "none")],
    );
    sh.assert_handbacks("the re-arming job's death is one handback", 1);
}

#[test]
fn bash_hands_a_killed_holders_modes_back_once() {
    let mut sh = Shell::boot("bi", BASH);
    sh.start(Job::Incident);
    sh.ctl.expect_on_screen("Killed");
    sh.await_modes("the incident is handed back", &handed_back("false"));
    sh.assert_handbacks("exactly one handback", 1);
}
