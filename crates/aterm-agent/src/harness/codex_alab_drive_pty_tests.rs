// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! A renewal's hand-back over a real pty: the steps aterm-gui's renewal worker takes once a
//! root ended with no successor (`HostClosed::hand_back`: [`crate::harness::relaunch::self_claimed_snapshot`]
//! then [`crate::harness::upgrade_drive::renewal_hand_back`]), on the live kernel, against a real
//! zsh whose stand-in codex-alab the renewal's Signal ended.

use super::*;

/// The stand-in's own word on the pty, each time it starts.
const UP: &str = "STAND-IN CODEX-ALAB UP";

/// A zsh on a pty of its own, its parent launchd's (an orphan, as a shell aterm's window opened
/// reads once its host is gone, [`crate::harness::upgrade_drive::owned_by_aterm`]), and everything it writes read.
struct OrphanZsh {
    master: i32,
    shell: u32,
    said: Arc<Mutex<Vec<u8>>>,
    drain: Option<std::thread::JoinHandle<()>>,
}

impl OrphanZsh {
    fn spawn(home: &Path, cwd: &Path) -> Self {
        use std::os::fd::IntoRawFd as _;
        use std::os::unix::process::CommandExt as _;
        let (master, slave, mut first) = aterm_pty::spawn_session_leader_on_pty(|slave| {
            let tty = || slave.try_clone().map(std::process::Stdio::from);
            let mut zsh = std::process::Command::new("/bin/zsh");
            zsh.args(["-f", "-i"])
                .env_clear()
                .env("HOME", home)
                .env("PATH", "/usr/bin:/bin")
                .env("TERM", "dumb")
                .env("PS1", "%% ")
                .env("ATERM_PARENT_SESSION_ID", TAB)
                .current_dir(cwd)
                .stdout(tty()?)
                .stderr(tty()?);
            // SAFETY: runs in the forked child before exec; calls only the async-signal-safe
            // `fork` and `_exit`. The first child exits at once, so the shell (which the pty's
            // own setup then makes its session's leader) is launchd's, as an orphan is.
            unsafe {
                zsh.pre_exec(|| match libc::fork() {
                    -1 => Err(std::io::Error::last_os_error()),
                    0 => Ok(()),
                    _ => libc::_exit(0),
                });
            }
            Ok(zsh)
        })
        .expect("the orphan zsh");
        drop(slave);
        assert!(first.wait().expect("the first child").success());
        let master = master.into_raw_fd();
        let said = Arc::new(Mutex::new(Vec::new()));
        // SAFETY: `master` is this test's open pty master; the duplicate is the drain's own.
        let read = unsafe { libc::fcntl(master, libc::F_DUPFD_CLOEXEC, 0) };
        assert!(read >= 0, "dup the master");
        let heard = Arc::clone(&said);
        let drain = std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                // SAFETY: reads into a local buffer from the drain's own descriptor.
                let n = unsafe { libc::read(read, buf.as_mut_ptr().cast(), buf.len()) };
                let Some(n) = usize::try_from(n).ok().filter(|n| *n > 0) else {
                    break;
                };
                heard.lock().unwrap().extend_from_slice(&buf[..n]);
            }
            aterm_pty::close_fd(read);
        });
        let mut zsh = Self {
            master,
            shell: 0,
            said,
            drain: Some(drain),
        };
        zsh.shell = zsh.until("the shell leads its pty, launchd's", || {
            let group = zsh.front()?;
            let row = aterm_sysprobe::kproc::record(group)?;
            (row.ppid == 1 && zsh.said().contains("% ")).then_some(group)
        });
        zsh
    }

    /// The pty's foreground group.
    fn front(&self) -> Option<u32> {
        // SAFETY: `tcgetpgrp` only reads the live master's foreground group.
        u32::try_from(unsafe { libc::tcgetpgrp(self.master) })
            .ok()
            .filter(|g| *g > 1)
    }

    fn said(&self) -> String {
        String::from_utf8_lossy(&self.said.lock().unwrap()).into_owned()
    }

    /// `look`'s answer, within the hang detector.
    fn until<T>(&self, what: &str, mut look: impl FnMut() -> Option<T>) -> T {
        let start = std::time::Instant::now();
        loop {
            if let Some(seen) = look() {
                return seen;
            }
            assert!(
                start.elapsed() < aterm_spec::wait::MUST_HAPPEN,
                "{what}: not within the hang detector; the pty said:\n{}",
                self.said()
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for OrphanZsh {
    fn drop(&mut self) {
        if self.shell != 0 {
            // SAFETY: the shell's pid, which this test's pty session still holds as its leader.
            unsafe { libc::kill(i32::try_from(self.shell).unwrap_or(0), libc::SIGKILL) };
        }
        aterm_pty::close_fd(self.master);
        if let Some(drain) = self.drain.take() {
            let _ = drain.join();
        }
    }
}

/// The runs `log` holds, as the stand-in wrote each: its pid, `CODEX_AUTH_PATH`, `CODEX_HOME`
/// and arguments.
fn runs(log: &Path) -> Vec<[String; 4]> {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .map(|line| {
            let mut words = line.splitn(4, '|').map(str::to_owned);
            std::array::from_fn(|_| words.next().unwrap_or_default())
        })
        .collect()
}

/// The text a `turn` request types: what follows its fixed words ([`crate::harness::upgrade_drive::TURN_WAIT`]).
fn turned_text(line: &str) -> Option<&str> {
    let (_, text) = line.split_once(&format!(" {} ", crate::harness::upgrade_drive::TURN_WAIT))?;
    Some(text)
}

/// THE HAND-BACK OF A SESSION A RENEWAL SIGNALLED WITH NO SUCCESSOR, END TO END: a real zsh on
/// a real pty ran a stand-in codex-alab (atpkg's launcher of a codex-alab package that reads
/// `CODEX_AUTH_PATH`) on the login the session ran on; the renewal's Signal ended it. aterm-gui's
/// worker then takes the steps `HostClosed::hand_back` takes, each a snapshot of the tab read
/// live (the shell's arguments and terminal, the pty's foreground group) and the crash rule's
/// relaunch under the renewal's cause: the shell runs the source's own resume line once, on its
/// own login and conversation, as the stand-in's own record of its environment shows; every
/// later hand-back for that source types nothing.
#[test]
fn a_renewal_hand_back_on_a_real_pty_runs_the_source_line_once_on_its_login() {
    let dir = scratch("renewal-hand-back-pty");
    let user = dir.join("home");
    let codex = user.join(".codex");
    let day = codex.join("sessions/2026/11/13");
    std::fs::create_dir_all(&day).expect("sessions");
    std::fs::create_dir_all(codex.join("thread-writer-locks")).expect("locks");
    let home = std::fs::canonicalize(&codex).expect("home");
    std::fs::write(
        day.join(format!("rollout-2026-11-13T08-00-00-{T2}.jsonl")),
        "{\"type\":\"session_meta\",\"payload\":{\"id\":\"x\",\"cwd\":\"/w\"}}\n",
    )
    .expect("rollout");
    let cwd = dir.join("w");
    std::fs::create_dir_all(&cwd).expect("cwd");
    let cwd = std::fs::canonicalize(&cwd).expect("cwd");
    let login = dir.join("accounts-codex/main/auth.json");
    std::fs::create_dir_all(login.parent().expect("its folder")).expect("login");
    std::fs::write(&login, "{}").expect("the login");
    let login = std::fs::canonicalize(&login)
        .expect("the login")
        .to_string_lossy()
        .into_owned();
    // The stand-in: atpkg's launcher of a codex-alab package whose binary holds the auth-path
    // mark. Its interpreter is named `codex`, as the kernel names a codex-alab's process.
    let interp = dir.join("sh/codex");
    std::fs::create_dir_all(interp.parent().expect("its folder")).expect("sh");
    std::os::unix::fs::symlink("/bin/sh", &interp).expect("the interpreter");
    let exe = crate::harness::codex_alab::lay_package(&dir.join("store/codex/1"), true);
    let log = dir.join("runs.log");
    std::fs::write(
        &exe,
        format!(
            "#!{interp}\n# {mark}\nprintf '%s|%s|%s|%s\\n' \"$$\" \"${{CODEX_AUTH_PATH-unset}}\" \
             \"${{CODEX_HOME-unset}}\" \"$*\" >> '{log}'\nprintf '{UP} %s\\n' \"$$\"\n\
             trap 'kill $! 2>/dev/null; exit 143' TERM\n/bin/sleep \"${{STAND_IN_FOR:-2}}\" & wait $!\n",
            interp = interp.display(),
            mark = crate::harness::codex_alab::AUTH_PATH_MARK,
            log = log.display(),
        ),
    )
    .expect("the stand-in");
    let twin = dir.join("prefix/agents/codex");
    std::fs::create_dir_all(twin.parent().expect("the twin's folder")).expect("agents");
    std::fs::write(&twin, crate::harness::codex_alab::twin_text(&exe)).expect("launcher");
    for program in [&exe, &twin] {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(program, std::fs::Permissions::from_mode(0o755)).expect("x");
    }
    let zsh = OrphanZsh::spawn(&user, &cwd);
    let master = zsh.master;
    // The tab's host, answering from the pty: its foreground group, and a `turn` typed into it.
    let (sock, asked) = crate::harness::fake_instance(&dir.join("t.sock"), TOKEN, move |line| {
        let verb = line
            .split_whitespace()
            .find(|w| !w.starts_with('@'))
            .unwrap_or("");
        match verb {
            "who" => {
                // SAFETY: `tcgetpgrp` only reads the live master's foreground group.
                let front = unsafe { libc::tcgetpgrp(master) };
                format!("OK 1\n1 {TAB} fgpgid={front}")
            }
            "sessions" => format!("OK 1\nlocal {TAB} 0 idle codex"),
            "text" => format!("OK {}", shell_after(&[])),
            "cell" => "OK A 9b9b9c 2d2f33 dim".to_string(),
            "status" => "OK schema=1 hold=0 hand=- integration=on".to_string(),
            "help" if line.contains("help key") => {
                "OK 2\nkey  key [if-gen=<epoch>.<seq>] [if=<re>] <name>\n     FENCES".to_string()
            }
            "help" => "OK 1\nturn [if-gen=<epoch>.<seq>] [if-human=<seq>] <text>".to_string(),
            "blocks" if line.contains("blocks 2") => format!("OK {}", blocks_after(0).0),
            "blocks" => format!("OK {}", blocks_after(0).1),
            "blocktext" => "OK 0".to_string(),
            "line" => "OK % ".to_string(),
            "lease" if line.contains(" acquire ") => "OK lease acquired".to_string(),
            "lease" => "OK lease released".to_string(),
            "turn" => match turned_text(line) {
                Some(text) => {
                    aterm_pty::write_all(master, format!("{text}\r").as_bytes());
                    "OK 0 turn submitted=1 status=settled seq=1 id=1".to_string()
                }
                None => "ERR unreadable turn".to_string(),
            },
            _ => "ERR unscripted".to_string(),
        }
    });
    let typed = || {
        asked
            .lock()
            .unwrap()
            .iter()
            .filter(|l| l.contains(" turn "))
            .cloned()
            .collect::<Vec<_>>()
    };
    // The session's agent, on its own login, as the person's shell started it.
    let quote = |p: &Path| format!("'{}'", p.display());
    aterm_pty::write_all(
        master,
        format!(
            "STAND_IN_FOR=60 CODEX_AUTH_PATH='{login}' CODEX_HOME={} {} resume {T2}\r",
            quote(&home),
            quote(&twin)
        )
        .as_bytes(),
    );
    let source = zsh.until("the source agent runs, leading the tab", || {
        let [pid, ..] = runs(&log).into_iter().next()?;
        let pid: u32 = pid.parse().ok()?;
        (zsh.front() == Some(pid)).then_some(pid)
    });
    let start = <Live as AlabKernel>::birth(&Live, source).expect("its kernel start");
    // The renewal's Signal; no successor ever takes the session.
    // SAFETY: the stand-in's pid, which leads the pty's foreground group this test watches.
    assert_eq!(
        unsafe { libc::kill(i32::try_from(source).unwrap(), libc::SIGTERM) },
        0
    );
    zsh.until("the shell holds the tab again", || {
        (zsh.front() == Some(zsh.shell) && !alive(source)).then_some(())
    });
    let resume = Resume {
        argv: vec![
            twin.to_string_lossy().into_owned(),
            "resume".to_string(),
            T2.to_string(),
        ],
        session: T2.to_string(),
        codex_home: Some(home.to_string_lossy().into_owned()),
        codex_auth_path: Some(login.clone()),
        cwd: Some(cwd.to_string_lossy().into_owned()),
    };
    let copy = crate::harness::self_claimed::Copy {
        lifetime: None,
        pid: source,
        start,
        home: home.clone(),
        seen: crate::harness::self_claimed::Seen::Never,
        resume: Ok(resume.clone()),
    };
    let opts = Opts {
        home: user.clone(),
        state: dir.join("state"),
        account_roster: None,
        sock: Some(sock),
        token: None,
        only_sid: Some(TAB.to_string()),
        dry_run: false,
        human_grace_s: 120,
        hand_back: false,
        background: false,
        aterm_state: None,
        ask: false,
        usage_tap: None,
        instances: crate::harness::upgrade_drive::Instances::Listed(Vec::new()),
        press: None,
        timings: crate::harness::Timings::FAST,
        agents_off: Vec::new(),
        land: false,
        tell: crate::harness::upgrade_drive::Tell::Off,
    };
    let witnesses = Witnesses::under(&opts.state);
    // `HostClosed::hand_back`'s step, which the renewal worker repeats while it waits or is
    // deferred and stops once the source is handled or the step ended.
    let step = || match crate::harness::relaunch::self_claimed_snapshot(&opts, &copy, &resume) {
        Ok(snapshot) => {
            crate::harness::upgrade_drive::renewal_hand_back(&opts, &snapshot, &resume).step
        }
        Err(why) => format!("snapshot:{why}"),
    };
    let mut steps = Vec::new();
    let handed = zsh.until("the hand-back ends", || {
        if witnesses.handled(source, &copy.start) {
            return Some(steps.last().cloned().unwrap_or_default());
        }
        let said = step();
        steps.push(said.clone());
        (!(said.starts_with("wait:") || said.starts_with("busy:"))).then_some(said)
    });
    assert_eq!(
        handed,
        "done:resumed",
        "{steps:?}\n{:#?}",
        asked.lock().unwrap()
    );
    let typed_once = typed();
    assert_eq!(typed_once.len(), 1, "one line typed: {typed_once:#?}");
    let ran = runs(&log);
    assert_eq!(ran.len(), 2, "the source, then its relaunch: {ran:?}");
    let [pid, auth, ran_home, args] = &ran[1];
    assert_ne!(pid, &source.to_string(), "a new process");
    assert_eq!(auth, &login, "the login the session ran on");
    assert_eq!(ran_home, &home.to_string_lossy(), "the home it ran on");
    assert_eq!(args, &format!("resume {T2}"), "the same conversation");
    assert!(witnesses.handled(source, &copy.start));
    assert_eq!(
        load(&opts, &key(TAB)).expect("the relaunch's record").cause,
        crate::harness::relaunch::CAUSE_RENEWAL
    );
    // The relaunched agent ends in its turn, the shell holds the tab again, and the worker (or a
    // second end of the same root) takes the step once more: the source is handled.
    let relaunched: u32 = pid.parse().expect("its pid");
    zsh.until("the relaunched stand-in ends", || {
        (!alive(relaunched) && zsh.front() == Some(zsh.shell)).then_some(())
    });
    assert_eq!(step(), "ended:handled");
    assert_eq!(typed(), typed_once, "the line is never typed twice");
    assert_eq!(runs(&log).len(), 2, "nothing ran again");
    assert_eq!(
        zsh.said().matches(UP).count(),
        2,
        "the pty saw the stand-in start twice:\n{}",
        zsh.said()
    );
}
