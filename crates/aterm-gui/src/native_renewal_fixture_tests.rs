// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::control::control_media::{Hops, MainPost, recv_main};

/// A stand-in PTY whose drop, the hang-up, sets a flag.
struct Hangup(Arc<AtomicBool>);

impl Drop for Hangup {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

/// A connection worker posts a session-status hop once the host has seen `stop`, after the live
/// reader's last pump, and waits for it as `call_main` does. The teardown answers it before it
/// hangs up the PTY, pumps nothing after that, and returns.
#[test]
fn codex_renewal_fixture_teardown_answers_a_late_status_hop_before_pty_hangup() {
    static HOPS: Hops = Hops::new();
    const SESSION: u64 = 8880;
    let root = crate::test_scratch::short_dir("renewal-teardown");
    let handle = crate::session_store::test_handle_on(SESSION, -1);
    let mut dispatcher = Pump::source_fixture(handle.clone(), Arc::clone(&handle.term));
    let (post_hop, hops) = std::sync::mpsc::channel::<Wake>();
    let (accepted, worker_started) = std::sync::mpsc::channel();
    let (answered, answers) = std::sync::mpsc::channel();
    let server_stop = Arc::new(AtomicBool::new(false));
    let hung_up = Arc::new(AtomicBool::new(false));
    let socket = root.path().join("c.sock");
    let listener = CtlListener::bind(&socket).unwrap();
    let worker_stop = Arc::clone(&server_stop);
    let worker_hung_up = Arc::clone(&hung_up);
    let server = spawn_fixture_server(listener, Arc::clone(&server_stop), move |_stream| {
        accepted.send(()).unwrap();
        let until = Instant::now() + MUST_HAPPEN;
        while !worker_stop.load(Ordering::Acquire) {
            assert!(
                Instant::now() < until,
                "the teardown never stopped the server"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        let mut post = MainPost::injected(|wake| post_hop.send(wake).is_ok(), &HOPS);
        let (reply, _hop) = post
            .ask(|reply| Wake::ReadSessionStatus {
                session: SESSION,
                reply,
            })
            .map_err(|error| io::Error::other(error.to_string()))?;
        let answer = recv_main(&reply).map_err(|error| error.to_string());
        answered
            .send((answer, worker_hung_up.load(Ordering::Acquire)))
            .unwrap();
        Ok(true)
    });
    let reader_hung_up = Arc::clone(&hung_up);
    let reader = std::thread::spawn(move || {
        // The real reader's read returns only once the shell is hung up.
        let until = Instant::now() + MUST_HAPPEN;
        while !reader_hung_up.load(Ordering::Acquire) {
            if Instant::now() >= until {
                return Err(io::Error::other("the PTY was never hung up"));
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        Ok(())
    });
    let client = CtlStream::connect(&socket).unwrap();
    worker_started
        .recv_timeout(MUST_HAPPEN)
        .expect("the server served the client");
    let opts = crate::harness_host::window_drive_opts(
        root.path().join("home"),
        root.path().join("state"),
        None,
    );
    let live_reader =
        live_snapshot::Reader::start(Arc::clone(&handle.ctx), -1, opts, root.path().to_path_buf())
            .unwrap();
    let serving = Serving {
        server_stop,
        server,
        pty_stop: Arc::new(AtomicBool::new(false)),
        pty: Hangup(Arc::clone(&hung_up)),
        runs: Vec::new(),
        reader,
    };
    serving
        .tear_down(Some(live_reader), || {
            assert!(
                !hung_up.load(Ordering::Acquire),
                "the host pumped after it hung up the PTY"
            );
            if let Ok(wake) = hops.recv_timeout(Duration::from_millis(10)) {
                dispatcher.dispatch(wake);
            }
        })
        .unwrap();
    let (answer, after_hangup) = answers
        .try_recv()
        .expect("the server joined its worker before the teardown returned");
    assert!(answer.unwrap().unwrap().contains("schema=1"));
    assert!(!after_hangup, "the hop was answered after the PTY hung up");
    drop(client);
}

/// A fixture config with every field it requires, and `launch` as given on the wire (`None`:
/// absent).
fn config_with(launch: Option<&str>) -> Result<NativeRenewalFixture, aterm_json::Error> {
    let launch = launch.map_or(String::new(), |launch| format!(",\"launch\":{launch}"));
    aterm_json::from_str(&format!(
        "{{\"root\":\"/r\",\"codex\":\"/r/codex\",\"cwd\":\"/r/work\",\
         \"auth_path\":\"/r/auth.json\",\"args\":[\"exec\"],\"env\":{{}}{launch}}}"
    ))
}

/// Each launch word parses to its shape, and `background` takes its runs; an absent `launch` is
/// the leader. Any other word, a word in another case or spelling, `background` without its runs
/// or with runs out of range, and runs on another shape each fail the parse.
#[test]
fn native_renewal_fixture_launch_parses_each_word_and_refuses_any_other() {
    use NativeRenewalLaunch as L;
    assert_eq!(config_with(None).unwrap().launch, L::Leader);
    for (wire, launch) in [
        ("\"leader\"", L::Leader),
        ("\"sh-child\"", L::ShChild),
        ("\"pipeline\"", L::Pipeline),
        ("{\"background\":2}", L::Background(LaunchRuns(2))),
        ("{\"background\":16}", L::Background(LaunchRuns(16))),
        ("\"setsid\"", L::Setsid),
        ("\"setsid-chain\"", L::SetsidChain),
    ] {
        assert_eq!(config_with(Some(wire)).unwrap().launch, launch, "{wire}");
    }
    assert_eq!(
        aterm_json::to_string(&L::Background(LaunchRuns(2))).unwrap(),
        "{\"background\":2}"
    );
    assert_eq!(aterm_json::to_string(&L::ShChild).unwrap(), "\"sh-child\"");
    for wire in [
        "\"tmux\"",
        "\"Leader\"",
        "\"sh_child\"",
        "\"\"",
        "\"background\"",
        "{\"background\":0}",
        "{\"background\":17}",
        "{\"setsid\":1}",
        "null",
    ] {
        assert!(config_with(Some(wire)).is_err(), "{wire} parsed");
    }
}

/// A leader's line, for an argv of printable ASCII, is the one the fixture typed before launch
/// shapes, byte for byte; a word with a NUL, which no argv can carry, refuses the line.
#[test]
fn native_renewal_fixture_leader_types_the_line_it_always_typed() {
    let (codex, record) = (Path::new("/r/bin/codex"), Path::new("/r/launch-record"));
    let exec = ["exec".into(), "--json".into(), "it's".into()];
    assert_eq!(
        NativeRenewalLaunch::Leader
            .line(codex, &exec, record)
            .unwrap(),
        "'/r/bin/codex' 'exec' '--json' 'it'\\''s'; printf '\\nEXEC_FIXTURE_EXIT=%s\\n' \"$?\"\n"
    );
    assert_eq!(
        NativeRenewalLaunch::Leader
            .line(codex, &["resume".into()], record)
            .unwrap(),
        "'/r/bin/codex' 'resume'\n"
    );
    assert!(
        NativeRenewalLaunch::ShChild
            .line(codex, &["a\0b".into()], record)
            .is_err()
    );
}

/// A host that adopts a PTY starts no run: it types nothing for the leader, and any other launch
/// shape, which it could not honour, refuses its config. A host that does not adopt types its
/// shape's line.
#[test]
fn native_renewal_fixture_adopting_host_takes_no_launch_shape() {
    let record = Path::new("/r/launch-record");
    let adopting = |launch: &str| -> NativeRenewalFixture {
        let mut config = config_with(Some(launch)).unwrap();
        config.adopt = Some(
            aterm_json::from_str(
                "{\"master\":3,\"shell_pid\":2,\"sid\":\"s\",\"nonce\":\"n\",\
                 \"prior_host_pid\":4,\"prior_host_birth\":5,\"waited_pid\":4,\
                 \"wait_outcome\":{\"kind\":\"exited\",\"code\":0}}",
            )
            .unwrap(),
        );
        config
    };
    assert_eq!(adopting("\"leader\"").launch_line(record), Ok(None));
    for launch in [
        "\"sh-child\"",
        "\"pipeline\"",
        "{\"background\":1}",
        "\"setsid\"",
        "\"setsid-chain\"",
    ] {
        assert!(
            adopting(launch).launch_line(record).is_err(),
            "an adopting host took {launch}"
        );
        let line = config_with(Some(launch)).unwrap().launch_line(record);
        assert!(
            line.as_ref().is_ok_and(|line| line
                .as_deref()
                .is_some_and(|line| line.contains("launch-record"))),
            "{launch}: {line:?}"
        );
    }
}

/// `prepare` makes the empty record each run but a leader's reports to, and refuses one already
/// there, which would name runs this host did not start; a setsid shape also refuses where its
/// helper's interpreter is missing, and no other shape needs it.
#[test]
fn native_renewal_fixture_prepare_refuses_a_record_already_there_and_a_missing_helper() {
    use NativeRenewalLaunch as L;
    let scratch = crate::test_scratch::short_dir("fixture-prepare");
    let record = scratch.path().join("launch-record");
    let missing = scratch.path().join("no-perl");
    L::Leader.prepare_with(&record, &missing).unwrap();
    assert!(!record.exists(), "a leader reports to no record");
    L::ShChild.prepare_with(&record, &missing).unwrap();
    assert_eq!(std::fs::read(&record).unwrap(), b"");
    let again = L::ShChild.prepare_with(&record, &missing);
    assert!(
        again.as_ref().is_err_and(|why| why.contains("exists")),
        "{again:?}"
    );
    std::fs::write(&record, "4242\n").unwrap();
    assert!(L::Pipeline.prepare_with(&record, &missing).is_err());
    assert_eq!(std::fs::read_to_string(&record).unwrap(), "4242\n");
    std::fs::remove_file(&record).unwrap();
    for setsid in [L::Setsid, L::SetsidChain] {
        let why = setsid.prepare_with(&record, &missing).unwrap_err();
        assert!(why.contains("no-perl"), "{why}");
        assert!(!record.exists(), "a refused shape made its record");
    }
}

/// `started` refuses a record that names more runs than the line starts, or a line that is no
/// pid; each run it read before such a line is already in its runs, for the teardown to end.
#[test]
fn native_renewal_fixture_started_refuses_a_record_it_cannot_read_as_its_runs() {
    use NativeRenewalLaunch as L;
    let scratch = crate::test_scratch::short_dir("fixture-started");
    let record = scratch.path().join("launch-record");
    let me = i32::try_from(std::process::id()).unwrap();
    let codex = |_: i32| Some(vec![OsString::from("/r/codex"), OsString::from("exec")]);
    let two = L::Background(LaunchRuns::try_from(2).unwrap());
    for (text, launch, refusal) in [
        (format!("{me}\n{me}\n"), L::ShChild, "more runs"),
        ("abc\n".to_owned(), L::ShChild, "no pid"),
        ("1\n".to_owned(), L::ShChild, "no pid"),
        (format!("{me}\nabc\n"), two, "no pid"),
    ] {
        std::fs::write(&record, &text).unwrap();
        let mut runs = Vec::new();
        let why = launch
            .started_reading(-1, -1, &record, &mut runs, codex)
            .unwrap_err();
        assert!(why.contains(refusal), "{text:?}: {why}");
        if text.starts_with(&me.to_string()) {
            assert_eq!(runs, [Run::observe(me).unwrap()], "{text:?}");
        }
    }
}

/// A run's pid reaches its record, as the foreground group's would, a moment before its process
/// execs codex. `started` answers it only once the argument record is no longer
/// [`RUN_REPORT`]'s `sh`.
#[test]
fn native_renewal_fixture_started_answers_a_run_once_it_execs_codex() {
    let scratch = crate::test_scratch::short_dir("fixture-execd");
    let record = scratch.path().join("launch-record");
    let me = i32::try_from(std::process::id()).unwrap();
    std::fs::write(&record, format!("{me}\n")).unwrap();
    let reporter = ["/bin/sh", "-c", RUN_REPORT, "fixture-run"].map(OsString::from);
    let mut asked = 0;
    let mut runs = Vec::new();
    let pids = NativeRenewalLaunch::ShChild
        .started_reading(-1, -1, &record, &mut runs, |pid| {
            assert_eq!(pid, me);
            asked += 1;
            match asked {
                1 | 3 => Some(reporter.to_vec()),
                // The exec's first moment, when the kernel does not hand over its record yet.
                2 | 4 => None,
                _ => Some(vec![OsString::from("/r/codex"), OsString::from("exec")]),
            }
        })
        .unwrap();
    assert_eq!(pids, [me]);
    assert_eq!(asked, 5, "started answered before the run ran codex");
}

/// What a test reads of one process of a launched tree.
#[cfg(target_vendor = "apple")]
#[derive(Clone, Copy, Debug)]
struct Facts {
    identity: aterm_uds::process::Identity,
    pid: i32,
    parent: i32,
    pgid: i32,
    /// `getsid`.
    sid: i32,
    /// Its session's controlling terminal.
    tty: Option<u32>,
}

#[cfg(target_vendor = "apple")]
impl Facts {
    fn of(pid: i32) -> Self {
        let lineage = aterm_uds::process::read_lineage(pid)
            .unwrap_or_else(|| panic!("{pid} is a live process of this user"));
        // SAFETY: `getsid` reads a process's session id and takes no pointers.
        let sid = unsafe { libc::getsid(pid) };
        assert!(sid > 0, "{pid}'s session reads");
        Self {
            identity: lineage.identity,
            pid,
            parent: i32::try_from(lineage.parent).unwrap(),
            pgid: i32::try_from(lineage.pgid).unwrap(),
            sid,
            tty: lineage.tty,
        }
    }

    /// `pid` and each of its ancestors below `shell`, nearest first: how many hops below the tab's
    /// shell it runs is the chain's length.
    fn chain(pid: i32, shell: i32) -> Vec<Self> {
        let mut chain = vec![Self::of(pid)];
        while chain.last().unwrap().parent != shell {
            assert!(
                chain.len() < 8,
                "{pid} runs under the tab's shell: {chain:?}"
            );
            chain.push(Self::of(chain.last().unwrap().parent));
        }
        chain
    }

    /// Whether this process is gone and reaped: no process answers its pid, or another process
    /// does. A zombie still answers, so this holds only once its parent reaped it.
    fn reaped(&self) -> bool {
        !aterm_uds::process::pid_alive(self.pid.unsigned_abs())
            || aterm_uds::process::read_identity(self.pid)
                .is_some_and(|now| now.birth != self.identity.birth)
    }

    /// `SIGTERM` or `SIGKILL` it while its pid still names it.
    fn signal(&self, signal: i32) {
        if aterm_uds::process::read_identity(self.pid)
            .is_some_and(|now| now.birth == self.identity.birth)
        {
            // SAFETY: `kill` posts a signal to the process just proven to be this one.
            unsafe { libc::kill(self.pid, signal) };
        }
    }
}

/// A real PTY running `zsh -f -i` as the host starts it, its master blocking as the host's is,
/// its output drained, inside the host's own [`Serving`]: a launch's runs join its runs as
/// `started` sees them, and [`Self::tear_down`] is the host's teardown. Dropped, it kills each
/// process of [`Self::tree`] still there, tears down and reaps the shell.
#[cfg(target_vendor = "apple")]
struct Tab {
    master: i32,
    shell_pid: i32,
    output: Arc<Mutex<Vec<u8>>>,
    serving: Option<Serving<FixturePty>>,
    /// Every process the launch started, as read while it ran.
    tree: Vec<Facts>,
}

#[cfg(target_vendor = "apple")]
impl Tab {
    fn spawn(root: &Path) -> Self {
        // SAFETY: the test owns this shell, its PTY and its scratch root.
        let authority = unsafe { aterm_cap::Authority::root_authority() };
        let spawn = authority.grant::<aterm_cap::effects::Spawn>(aterm_cap::Tier::Trusted);
        let sandbox = authority.grant::<aterm_sandbox::Sandbox>(aterm_cap::Tier::Trusted);
        let env = [("HOME".to_owned(), root.to_string_lossy().into_owned())];
        let shell_args: [String; 2] = ["-f".into(), "-i".into()];
        let shell = aterm_pty::spawn_shell_with_pid(
            40,
            140,
            &spawn,
            &sandbox,
            &env,
            Some("/bin/zsh"),
            Some(&shell_args),
            None,
            None,
            root.to_str(),
            None,
            aterm_sandbox::Limits::inherit(),
        )
        .expect("the fixture's shell starts");
        let output = Arc::<Mutex<Vec<u8>>>::default();
        let pty_stop = Arc::<AtomicBool>::default();
        let (master, drained, stopped) = (shell.master, Arc::clone(&output), Arc::clone(&pty_stop));
        // The master blocks, so the reader waits on `poll` and looks at `stopped` before each
        // read: once the teardown sets it and closes the master, nothing reads that number.
        let reader = std::thread::spawn(move || {
            let mut bytes = [0u8; 4096];
            while !stopped.load(Ordering::Acquire) {
                let mut ready = libc::pollfd {
                    fd: master,
                    events: libc::POLLIN,
                    revents: 0,
                };
                // SAFETY: `poll` reads and writes the one `pollfd` it is given.
                let polled = unsafe { libc::poll(&mut ready, 1, 5) };
                if stopped.load(Ordering::Acquire) {
                    break;
                }
                let n = if polled > 0 {
                    aterm_pty::read(master, &mut bytes)
                } else {
                    0
                };
                if n > 0 {
                    drained
                        .lock()
                        .unwrap()
                        .extend_from_slice(&bytes[..n.unsigned_abs()]);
                } else if polled > 0 {
                    // Hung up: `poll` answers at once from here on.
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
            Ok(())
        });
        let serving = Serving {
            server_stop: Arc::default(),
            server: std::thread::spawn(|| Ok(())),
            pty_stop,
            pty: FixturePty {
                master,
                shell: aterm_pty::record_spawned_shell(shell.pid),
            },
            runs: Vec::new(),
            reader,
        };
        Self {
            master,
            shell_pid: shell.pid,
            output,
            serving: Some(serving),
            tree: Vec::new(),
        }
    }

    fn output(&self) -> String {
        String::from_utf8_lossy(&self.output.lock().unwrap()).replace('\r', "")
    }

    /// The runs the host would end at its teardown.
    fn runs(&mut self) -> &mut Vec<Run> {
        &mut self.serving.as_mut().expect("the tab still serves").runs
    }

    /// The host's teardown: each run ended, the shell hung up, the master closed.
    fn tear_down(&mut self) -> Result<(), String> {
        self.serving
            .take()
            .map_or(Ok(()), |serving| serving.tear_down(None, || {}))
    }
}

#[cfg(target_vendor = "apple")]
impl Drop for Tab {
    fn drop(&mut self) {
        for process in &self.tree {
            process.signal(libc::SIGKILL);
        }
        let _ = self.tear_down();
        let reaped = aterm_spec::wait::within(MUST_HAPPEN, Duration::from_millis(5), || {
            aterm_pty::collect_exit_status(self.shell_pid)
        });
        if reaped.is_none() {
            // SAFETY: ending and reaping the test's own child, which outlived its hang-up.
            unsafe {
                libc::kill(self.shell_pid, libc::SIGKILL);
                libc::waitpid(self.shell_pid, std::ptr::null_mut(), 0);
            }
        }
    }
}

/// The stand-in codex: it records its argv, each word NUL-ended, in `argv-<pid>` beside itself,
/// then becomes `sleep` in its own place, so its pid stays the run's.
#[cfg(target_vendor = "apple")]
const STAND_IN: &str = "#!/bin/sh\n\
    for word in \"$@\"; do printf '%s\\0' \"$word\"; done >\"${0%/*}/argv-$$.part\" \
    && mv \"${0%/*}/argv-$$.part\" \"${0%/*}/argv-$$\" && exec /bin/sleep 600\n";

/// An argv no shell may parse on its way to codex: quotes, `$`, a command substitution, a
/// backquote, `;`, `!`, a tab, a newline and a non-ASCII letter; a word that is typed as `$'…'`
/// holding a single quote, a backslash and a DEL, each of which its form must escape; and a prompt
/// longer than the 1024 bytes the line discipline keeps of a line typed in canonical mode. A shell
/// that parsed it would make `pwned` in the tab's working directory.
#[cfg(target_vendor = "apple")]
fn hostile_argv() -> Vec<String> {
    vec![
        "exec".into(),
        "it's \"quoted\" $HOME $(touch pwned) `touch pwned` ; touch pwned !! \\".into(),
        "a\ttab, a\nnewline, !! and é".into(),
        "é' ; touch pwned ; echo '\\' ; touch pwned ; echo 'a\x7fb".into(),
        "a long prompt, ".repeat(140),
    ]
}

/// Start `launch`'s runs of the stand-in codex with [`hostile_argv`] in a fresh tab, through the
/// host's own `line`, `prepare`, [`type_line`] and `started`, and require each run's argv to
/// arrive whole. Answer the tab and each run's [`Facts::chain`], each process of which the tab
/// kills if the test fails.
#[cfg(target_vendor = "apple")]
fn launch_in_tab(launch: NativeRenewalLaunch, root: &Path) -> (Tab, Vec<Vec<Facts>>) {
    use std::os::unix::fs::PermissionsExt as _;
    if matches!(
        launch,
        NativeRenewalLaunch::Setsid | NativeRenewalLaunch::SetsidChain
    ) && !Path::new(SETSID[0]).is_file()
    {
        aterm_spec::host::could_not_run(format!(
            "no {} here, the interpreter the setsid helper runs in",
            SETSID[0]
        ));
    }
    let codex = root.join("codex");
    std::fs::write(&codex, STAND_IN).unwrap();
    std::fs::set_permissions(&codex, std::fs::Permissions::from_mode(0o755)).unwrap();
    let record = root.join("launch-record");
    let argv = hostile_argv();
    let line = launch.line(&codex, &argv, &record).unwrap();
    assert!(
        line.len() > 2048,
        "the line outgrows a canonical line: {} bytes",
        line.len()
    );
    let mut tab = Tab::spawn(root);
    launch.prepare(&record).unwrap();
    type_line(tab.master, &line)
        .unwrap_or_else(|why| panic!("{why}; the tab shows:\n{}", tab.output()));
    let (master, shell_pid) = (tab.master, tab.shell_pid);
    let pids = launch
        .started(master, shell_pid, &record, tab.runs())
        .unwrap_or_else(|why| panic!("{why}; the tab shows:\n{}", tab.output()));
    assert_eq!(pids.len(), launch.runs(), "one pid per run: {pids:?}");
    let expected = argv
        .iter()
        .map(|word| format!("{word}\0"))
        .collect::<String>();
    for pid in &pids {
        let recorded = root.join(format!("argv-{pid}"));
        aterm_spec::wait::within(MUST_HAPPEN, Duration::from_millis(5), || {
            recorded.exists().then_some(())
        })
        .unwrap_or_else(|| {
            panic!(
                "run {pid} never ran codex; the tab shows:\n{}",
                tab.output()
            )
        });
        assert_eq!(
            std::fs::read_to_string(&recorded).unwrap(),
            expected,
            "run {pid}'s argv"
        );
    }
    let runs = pids
        .iter()
        .map(|pid| Facts::chain(*pid, shell_pid))
        .collect::<Vec<_>>();
    tab.tree = runs.iter().flatten().copied().collect();
    (tab, runs)
}

/// Each status line the tab shows for an exec run that ended by `SIGTERM`: the run's pid where
/// the shell names it (`EXEC_FIXTURE_RUN=<pid>` on the line before), else none.
#[cfg(target_vendor = "apple")]
fn terminated(output: &str) -> Vec<Option<i32>> {
    let lines = output.lines().map(str::trim).collect::<Vec<_>>();
    lines
        .iter()
        .enumerate()
        .filter(|(_, line)| **line == "EXEC_FIXTURE_EXIT=143")
        .map(|(at, _)| {
            at.checked_sub(1)
                .and_then(|before| lines[before].strip_prefix("EXEC_FIXTURE_RUN="))
                .and_then(|pid| pid.parse().ok())
        })
        .collect()
}

/// [`launch_in_tab`], then hand `check` the shell's pid, the PTY's foreground group and each run's
/// chain. End the runs one at a time, the last started first, and require the shell's status
/// line for each as it ends, naming its run where there are several, while each earlier run still
/// goes; then every process the launch started gone and reaped, and no `pwned`.
#[cfg(target_vendor = "apple")]
fn launched_tree(launch: NativeRenewalLaunch, check: impl FnOnce(i32, i32, &[Vec<Facts>])) {
    let scratch = crate::test_scratch::short_dir("fixture-launch");
    let root = std::fs::canonicalize(scratch.path()).unwrap();
    let (tab, runs) = launch_in_tab(launch, &root);
    check(
        tab.shell_pid,
        crate::quit_safety::foreground_pgrp(tab.master),
        &runs,
    );
    for (ending, run) in runs.iter().enumerate().rev() {
        run[0].signal(libc::SIGTERM);
        let named = (runs.len() > 1).then_some(run[0].pid);
        let reported = |output: &str| {
            let statuses = terminated(output);
            statuses.len() == runs.len() - ending && statuses.last() == Some(&named)
        };
        aterm_spec::wait::within(MUST_HAPPEN, Duration::from_millis(5), || {
            reported(&tab.output()).then_some(())
        })
        .unwrap_or_else(|| {
            panic!(
                "run {}'s status never reached the tab:\n{}",
                run[0].pid,
                tab.output()
            )
        });
        for earlier in &runs[..ending] {
            assert!(!earlier[0].reaped(), "run {} still goes", earlier[0].pid);
        }
    }
    let left = || tab.tree.iter().filter(|process| !process.reaped()).count();
    aterm_spec::wait::within(MUST_HAPPEN, Duration::from_millis(5), || {
        (left() == 0).then_some(())
    })
    .unwrap_or_else(|| panic!("{} launched processes outlived their runs", left()));
    assert!(!root.join("pwned").exists(), "a shell parsed codex's argv");
}

/// The host's teardown ends a run in a session of its own, which the shell's hang-up never
/// reaches: the setsid shapes' codex and the session leader above it, with every other process
/// the launch started, are gone once it returns and its shell is hung up.
#[cfg(target_vendor = "apple")]
#[test]
fn native_renewal_fixture_teardown_ends_a_run_the_hangup_cannot_reach() {
    if !crate::test_alone::alone() {
        return;
    }
    for launch in [
        NativeRenewalLaunch::Setsid,
        NativeRenewalLaunch::SetsidChain,
    ] {
        let scratch = crate::test_scratch::short_dir("fixture-teardown");
        let root = std::fs::canonicalize(scratch.path()).unwrap();
        let (mut tab, runs) = launch_in_tab(launch, &root);
        let [run] = runs.as_slice() else {
            panic!("one run: {runs:?}")
        };
        assert_eq!(
            run[0].tty, None,
            "{launch:?}: codex has no controlling terminal"
        );
        tab.tear_down().unwrap();
        let left = || {
            run.iter()
                .filter(|process| !process.reaped())
                .map(|process| process.pid)
                .collect::<Vec<_>>()
        };
        aterm_spec::wait::within(MUST_HAPPEN, Duration::from_millis(5), || {
            left().is_empty().then_some(())
        })
        .unwrap_or_else(|| panic!("{launch:?}: {:?} outlived the teardown", left()));
    }
}

/// A config without `launch` starts codex as the PTY's foreground leader, the shell's own child,
/// as the fixture always did.
#[cfg(target_vendor = "apple")]
#[test]
fn native_renewal_fixture_launch_default_leads_the_foreground_group() {
    if !crate::test_alone::alone() {
        return;
    }
    launched_tree(
        config_with(None).unwrap().launch,
        |shell, foreground, runs| {
            let [run] = runs else {
                panic!("one run: {runs:?}")
            };
            let [codex] = run.as_slice() else {
                panic!("codex is the shell's child: {run:?}")
            };
            assert_eq!(codex.pid, foreground, "codex leads the foreground group");
            assert_eq!((codex.pgid, codex.sid), (codex.pid, shell));
        },
    );
}

/// `sh-child`: codex is a child of the `sh -c` that leads the foreground group, in its group and
/// the PTY's session, two hops below the shell.
#[cfg(target_vendor = "apple")]
#[test]
fn native_renewal_fixture_launch_sh_child_runs_codex_under_the_foreground_leader() {
    if !crate::test_alone::alone() {
        return;
    }
    launched_tree(NativeRenewalLaunch::ShChild, |shell, foreground, runs| {
        let [run] = runs else {
            panic!("one run: {runs:?}")
        };
        let [codex, sh] = run.as_slice() else {
            panic!("codex runs two hops below the shell: {run:?}")
        };
        assert_ne!(
            codex.pid, foreground,
            "codex does not lead the foreground group"
        );
        assert_eq!(sh.pid, foreground, "its sh leads it");
        assert_eq!((codex.pgid, codex.sid, sh.sid), (foreground, shell, shell));
        assert!(codex.tty.is_some(), "codex keeps the PTY as its terminal");
    });
}

/// `pipeline`: codex is the shell's child, a member of the foreground group the pipeline's first
/// stage leads.
#[cfg(target_vendor = "apple")]
#[test]
fn native_renewal_fixture_launch_pipeline_runs_codex_as_a_later_stage() {
    if !crate::test_alone::alone() {
        return;
    }
    launched_tree(NativeRenewalLaunch::Pipeline, |shell, foreground, runs| {
        let [run] = runs else {
            panic!("one run: {runs:?}")
        };
        let [codex] = run.as_slice() else {
            panic!("codex is the shell's child: {run:?}")
        };
        assert_ne!(
            codex.pid, foreground,
            "codex does not lead the foreground group"
        );
        assert_ne!(foreground, shell, "the pipeline is the foreground job");
        assert_eq!((codex.pgid, codex.sid), (foreground, shell));
    });
}

/// `background` with two runs: two codex processes in one PTY, each the shell's child leading a
/// background group of its own, while the shell's group holds the foreground. The second run
/// ends first, and its status, naming it, reaches the tab while the first still goes.
#[cfg(target_vendor = "apple")]
#[test]
fn native_renewal_fixture_launch_background_starts_each_run_in_one_pty() {
    if !crate::test_alone::alone() {
        return;
    }
    let two = NativeRenewalLaunch::Background(LaunchRuns::try_from(2).unwrap());
    launched_tree(two, |shell, foreground, runs| {
        let [first, second] = runs else {
            panic!("two runs: {runs:?}")
        };
        assert_ne!(first[0].pid, second[0].pid);
        assert_eq!(foreground, shell, "the waiting shell holds the foreground");
        for run in [first, second] {
            let [codex] = run.as_slice() else {
                panic!("codex is the shell's child: {run:?}")
            };
            assert_ne!(
                codex.pid, foreground,
                "codex does not lead the foreground group"
            );
            assert_eq!((codex.pgid, codex.sid), (codex.pid, shell));
            assert_eq!(codex.tty, first[0].tty, "both runs share the PTY");
            assert!(codex.tty.is_some());
        }
    });
}

/// `setsid`: codex is a child of the session leader the helper made, three hops below the shell,
/// in another session with no controlling terminal; the helper waiting for it leads the
/// foreground group.
#[cfg(target_vendor = "apple")]
#[test]
fn native_renewal_fixture_launch_setsid_runs_codex_in_a_session_of_its_own() {
    if !crate::test_alone::alone() {
        return;
    }
    launched_tree(NativeRenewalLaunch::Setsid, |shell, foreground, runs| {
        let [run] = runs else {
            panic!("one run: {runs:?}")
        };
        let [codex, leader, waiter] = run.as_slice() else {
            panic!("codex runs three hops below the shell: {run:?}")
        };
        assert_ne!(
            codex.pid, foreground,
            "codex does not lead the foreground group"
        );
        assert_ne!(codex.sid, shell, "codex is in another session");
        assert_eq!((codex.sid, leader.sid), (leader.pid, leader.pid));
        assert_eq!(codex.tty, None, "codex has no controlling terminal");
        assert_eq!((waiter.pid, waiter.sid), (foreground, shell));
    });
}

/// `setsid-chain`: a tool shell's `sh -c 'setsid sh -c "codex …"'`, codex four hops below the
/// shell, a child of the session leader the helper made, whose parent and grandparent are in the
/// PTY's session, the grandparent leading the foreground group.
#[cfg(target_vendor = "apple")]
#[test]
fn native_renewal_fixture_launch_setsid_chain_runs_codex_four_hops_down() {
    if !crate::test_alone::alone() {
        return;
    }
    launched_tree(
        NativeRenewalLaunch::SetsidChain,
        |shell, foreground, runs| {
            let [run] = runs else {
                panic!("one run: {runs:?}")
            };
            let [codex, leader, sh, tool_shell] = run.as_slice() else {
                panic!("codex runs four hops below the shell: {run:?}")
            };
            assert_ne!(
                codex.pid, foreground,
                "codex does not lead the foreground group"
            );
            assert_ne!(codex.sid, shell, "codex is in another session");
            assert_eq!((codex.sid, leader.sid), (leader.pid, leader.pid));
            assert_eq!(codex.tty, None, "codex has no controlling terminal");
            assert_eq!((sh.sid, sh.pgid), (shell, foreground));
            assert_eq!((tool_shell.pid, tool_shell.sid), (foreground, shell));
        },
    );
}
