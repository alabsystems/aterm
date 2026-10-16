// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! A SEED WITHOUT THE INSTALL CONSENT ADOPTS NOTHING, at the real process edge.
//!
//! A seed that is switched off must notice before it writes `<prefix>/adopted`, lays a
//! pending stub under `bin/` and `agents/`, lays the reroute stubs, writes the shell hooks
//! under `~/.aterm` or the rc block, or reasserts the rustup seam: every stub would promise
//! a pass the switch refuses, and the adoption would install the whole set the first tick
//! after the switch was lifted. The switch is the ONE install consent, `[packages]
//! auto_install = false` in a scratch aterm.toml, and a recorded decline outranks it. The
//! store stays untouched — then, as the non-vacuity half, the SAME fixture with the
//! default config adopts, so the first half cannot pass merely because the fixture never
//! reaches the adoption line.
//!
//! AND A CONSENTING SEED MAKES THE TOOLCHAIN REACHABLE AT ONCE (2026-10-15): the shell
//! hooks are written by the seed, not a minute later at the end of the first update pass,
//! they append `bin/` behind the caller's PATH, and `bin/` already answers `targo` — a
//! stub that says `trust`'s state, and waits for it while a pass installs it or, before
//! that pass begins, while the window's seed has promised it (`pass-owed.json`).
//!
//! NEVER THE REAL STORE OR MACHINE. HOME is a temp directory (so the default prefix
//! and `~/.rustup` sit under it, and the machine-settings lane refuses a synthetic
//! home), XDG_CONFIG_HOME is a temp directory holding the scratch config, and the
//! registry is an empty local dir (no network) — except for the one test that watches the
//! network fetcher itself, whose every request goes to a local proxy that counts the
//! connection and drops it, so nothing leaves the machine.

#![cfg(unix)]
use aterm_uds::spawnfd::{Child, Command, Stdio};
use std::path::PathBuf;
use std::process::Output;

struct Fixture {
    root: aterm_tempfile::TempDir,
    home: PathBuf,
    layout: atpkg::store::Layout,
}

impl Fixture {
    fn new(case: &str) -> Self {
        let root = aterm_tempfile::Builder::new()
            .prefix(&format!("atpkg-seed-disabled-{case}-"))
            .tempdir()
            .unwrap();
        let home = root.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        // An rc to wire: atpkg edits one only when it already exists.
        std::fs::write(home.join(".zshrc"), "").unwrap();
        std::fs::create_dir_all(root.path().join("registry")).unwrap();
        let prefix = atpkg::store::default_prefix(&home);
        assert!(
            prefix.starts_with(&home),
            "fixture: the default prefix must sit under the temp HOME: {}",
            prefix.display()
        );
        Self {
            root,
            home,
            layout: atpkg::store::Layout { prefix },
        }
    }

    fn seed(&self, declined: bool) -> Output {
        let config = self.root.path().join("config");
        std::fs::create_dir_all(config.join("aterm")).unwrap();
        std::fs::write(
            config.join("aterm/aterm.toml"),
            if declined {
                "[packages]\nauto_install = false\n"
            } else {
                ""
            },
        )
        .unwrap();
        self.seed_with(&[], &[])
    }

    /// `atpkg seed` on the config as it stands, with `args` after the verb and `envs` set.
    fn seed_with(&self, args: &[&str], envs: &[(&str, &str)]) -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_atpkg"));
        cmd.arg("seed")
            .args(args)
            .envs(envs.iter().copied())
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", self.root.path().join("config"))
            .env(
                "ATPKG_REGISTRY",
                format!("dir:{}", self.root.path().join("registry").display()),
            )
            .env_remove("RUSTUP_HOME")
            .env_remove("ATERM_CHILD")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        cmd.output().expect("run dev atpkg seed")
    }

    /// Everything the adoption lane writes, by the crate's own path accessors — the shell
    /// hooks' `~/.aterm` (and the rc ledger in it) included.
    fn adoption_writes(&self) -> Vec<PathBuf> {
        [
            self.layout.adopted(),
            self.layout.bin_dir(),
            self.layout.agents_dir(),
            atpkg::reroute::dir(&self.layout),
            self.home.join(".rustup"),
            self.home.join(".aterm"),
        ]
        .into_iter()
        .filter(|p| std::fs::symlink_metadata(p).is_ok())
        .collect()
    }

    /// The scratch `~/.zshrc`, as it stands.
    fn zshrc(&self) -> String {
        std::fs::read_to_string(self.home.join(".zshrc")).unwrap()
    }

    /// The zsh hook the seed writes.
    fn zsh_hook(&self) -> PathBuf {
        self.home.join(".aterm/shell.d/00-atpkg.zsh")
    }
}

/// `zsh -f` with nothing but `PATH=/usr/bin:/bin` and the scratch HOME, sourcing the seed's
/// hook and then running `script`: what an ordinary shell outside aterm makes of it.
fn in_hooked_zsh(fx: &Fixture, script: &str) -> std::io::Result<Output> {
    Command::new("/bin/zsh")
        .arg("-f")
        .arg("-c")
        .arg(format!(". '{}'; {script}", fx.zsh_hook().display()))
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", &fx.home)
        .stdin(Stdio::null())
        .output()
}

fn describe(out: &Output) -> String {
    format!(
        "status {:?}\nstdout:\n{}\nstderr:\n{}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

#[test]
fn a_declined_seed_adopts_nothing_and_lays_no_stubs() {
    let fx = Fixture::new("declined");
    let out = fx.seed(true);
    let text = describe(&out);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "a declined seed is a quiet skip: {text}"
    );
    assert!(
        stdout.contains("[packages].auto_install = false — not installing"),
        "the skip says why: {text}"
    );
    assert_eq!(
        fx.adoption_writes(),
        Vec::<PathBuf>::new(),
        "`auto_install = false` must stop seed before adoption, stubs, reroutes, shell \
         hooks or the rustup seam: {text}"
    );
    assert_eq!(fx.zshrc(), "", "no rc block either: {text}");

    // NON-VACUITY: the same fixture with the default config reaches the
    // adoption line and lays the stubs, so the empty lists above are the gate's doing.
    // (A build that pins no root key is disabled either way and has nothing to show.)
    if atpkg::manager_enabled_with(atpkg::PKG_TRUST_ANCHORS) {
        let fx = Fixture::new("enabled");
        let out = fx.seed(false);
        let text = describe(&out);
        assert!(
            fx.layout.adopted().is_file(),
            "an enabled seed adopts: {text}"
        );
        assert!(
            fx.layout.bin_dir().is_dir(),
            "an enabled seed lays pending stubs: {text}"
        );
        assert!(fx.zsh_hook().is_file(), "and the shell hooks: {text}");
    }
}

/// A RECORDED DECLINE (`uninstall --all`) outranks the default consent: the seed says so
/// and writes nothing — no adoption, no stub, no hook, no rc block.
#[test]
fn a_seed_on_a_declined_store_writes_nothing() {
    let fx = Fixture::new("decline-marker");
    std::fs::create_dir_all(&fx.layout.prefix).unwrap();
    std::fs::write(fx.layout.declined(), "# declined\n").unwrap();
    let out = fx.seed(false);
    let text = describe(&out);
    assert!(out.status.success(), "{text}");
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("removed on this machine"),
        "the skip says why: {text}"
    );
    assert_eq!(fx.adoption_writes(), Vec::<PathBuf>::new(), "{text}");
    assert_eq!(fx.zshrc(), "", "{text}");
}

/// THE SEED WIRES THE SHELL: the hooks and the rc block are written by the seed itself; an
/// ordinary shell that sources the hook gets `bin/` APPENDED behind its own PATH (never in
/// front of it), and `targo` resolves there at once — to a stub that says `trust`'s state.
/// A block the user deleted stays deleted on the next seed (the opt-out the block itself
/// documents).
#[test]
fn a_consenting_seed_puts_bin_on_path_and_answers_targo() {
    if !atpkg::manager_enabled_with(atpkg::PKG_TRUST_ANCHORS) {
        return; // A build that pins no root key adopts nothing (the test above).
    }
    let fx = Fixture::new("hooks");
    let out = fx.seed(false);
    let text = describe(&out);
    assert!(out.status.success(), "{text}");
    assert!(fx.zsh_hook().is_file(), "the seed writes the hooks: {text}");
    assert!(
        fx.zshrc().contains("atpkg shell integration"),
        "and wires the rc that exists: {text}"
    );

    let bin = fx.layout.bin_dir();
    match in_hooked_zsh(&fx, "print -r -- $PATH; whence -p targo") {
        Ok(shell) => {
            assert!(shell.status.success(), "{}", describe(&shell));
            let said = String::from_utf8_lossy(&shell.stdout);
            let mut lines = said.lines();
            assert_eq!(
                lines.next(),
                Some(format!("/usr/bin:/bin:{}", bin.display()).as_str()),
                "bin/ is appended, never prepended: {}",
                describe(&shell)
            );
            assert_eq!(
                lines.next(),
                Some(bin.join("targo").to_string_lossy().as_ref()),
                "`targo` resolves to the managed bin/ before trust lands: {}",
                describe(&shell)
            );
        }
        Err(e)
            if e.kind() == std::io::ErrorKind::NotFound
                && std::fs::symlink_metadata("/bin/zsh")
                    .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
        {
            eprintln!("unavailable: /bin/zsh is absent; shell-hook execution was not checked");
        }
        Err(e) => panic!("run zsh: {e}"),
    }

    // Run as a tool call runs it: no terminal, no pass. It says trust's state and asks
    // for trust — the program whose install lays it — and the command did not run.
    let ran = Command::new(bin.join("targo"))
        .arg("--version")
        .env("HOME", &fx.home)
        .env("XDG_CONFIG_HOME", fx.root.path().join("config"))
        .env("PATH", "/usr/bin:/bin")
        .stdin(Stdio::null())
        .output()
        .expect("run the targo stub");
    let ran_text = describe(&ran);
    assert_eq!(ran.status.code(), Some(127), "{ran_text}");
    assert!(
        String::from_utf8_lossy(&ran.stdout).contains("targo comes with trust"),
        "{ran_text}"
    );
    assert_eq!(
        std::fs::read_to_string(fx.layout.bump_file()).unwrap_or_default(),
        "trust unattended\n",
        "the bump names the program, which the installer can act on, for a script: \
         {ran_text}"
    );

    // The opt-out: the user deletes the block, and the next seed leaves the rc alone.
    std::fs::write(fx.home.join(".zshrc"), "").unwrap();
    let again = fx.seed(false);
    assert!(again.status.success(), "{}", describe(&again));
    assert_eq!(
        fx.zshrc(),
        "",
        "a deleted block is not re-added: {}",
        describe(&again)
    );
}

/// A pass in flight on `progress`, its plan `program` alone, downloading: this test process
/// stands in for it, its heartbeat kept fresh by a thread until the returned flag is raised,
/// as a real pass's is.
fn fake_pass(
    progress: PathBuf,
    program: &'static str,
) -> (
    std::sync::Arc<std::sync::atomic::AtomicBool>,
    std::thread::JoinHandle<()>,
) {
    let write_progress = move || {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let body = format!(
            "{{\"v\":1,\"pid\":{},\"pass\":\"net\",\"heartbeat_unix\":{now},\
             \"overall\":{{\"programs_done\":0,\"programs_total\":1}},\
             \"queue\":[\"{program}\"],\"programs\":{{\"{program}\":{{\"phase\":\"download\",\
             \"bytes_done\":1,\"bytes_total\":4}}}}}}",
            std::process::id()
        );
        let tmp = progress.with_extension("tmp");
        std::fs::write(&tmp, body).unwrap();
        std::fs::rename(&tmp, &progress).unwrap();
    };
    write_progress();
    let landed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let heartbeat = {
        let landed = std::sync::Arc::clone(&landed);
        std::thread::spawn(move || {
            while !landed.load(std::sync::atomic::Ordering::SeqCst) {
                write_progress();
                std::thread::sleep(std::time::Duration::from_millis(500));
            }
        })
    };
    (landed, heartbeat)
}

/// The fixture's `targo` stub run as a tool call runs it — no terminal — with its stderr
/// lines streamed to the returned receiver.
fn unattended_targo(fx: &Fixture) -> (Child, std::sync::mpsc::Receiver<String>) {
    unattended_targo_under(fx, &[])
}

/// [`unattended_targo`] run by `wrapper` (a program and its arguments, the stub's path and
/// `--version` after them), or directly when it is empty.
fn unattended_targo_under(
    fx: &Fixture,
    wrapper: &[&str],
) -> (Child, std::sync::mpsc::Receiver<String>) {
    use std::io::BufRead as _;
    let targo = fx.layout.bin_dir().join("targo");
    let mut command = match wrapper.split_first() {
        Some((program, args)) => {
            let mut command = Command::new(program);
            command.args(args).arg(&targo);
            command
        }
        None => Command::new(&targo),
    };
    let mut child = command
        .arg("--version")
        .env("HOME", &fx.home)
        .env("XDG_CONFIG_HOME", fx.root.path().join("config"))
        .env("PATH", "/usr/bin:/bin")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run the targo stub");
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    let stderr = child.stderr.take().unwrap();
    std::thread::spawn(move || {
        for line in std::io::BufReader::new(stderr)
            .lines()
            .map_while(Result::ok)
        {
            let _ = tx.send(line);
        }
    });
    (child, rx)
}

/// Hang detector, not a latency budget: the most any step below may take.
const HANG: std::time::Duration = std::time::Duration::from_secs(60);

/// The first line on `rx` that contains `needle`.
fn line_with(rx: &std::sync::mpsc::Receiver<String>, needle: &str) -> String {
    loop {
        let line = rx
            .recv_timeout(HANG)
            .unwrap_or_else(|_| panic!("hang detector: the stub never said {needle:?}"));
        if line.contains(needle) {
            return line;
        }
    }
}

/// trust lands: its build's `targo`, and the shim renamed over the stub.
fn land_targo(fx: &Fixture) {
    use std::os::unix::fs::PermissionsExt as _;
    let shim = fx.layout.bin_dir().join("targo");
    let build = fx.layout.prefix.join("store/trust/1/bin");
    std::fs::create_dir_all(&build).unwrap();
    let real = build.join("targo");
    std::fs::write(&real, "#!/bin/sh\necho \"ran targo $*\"\n").unwrap();
    std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o755)).unwrap();
    let laid = shim.with_file_name(".targo.lay-test");
    std::fs::write(
        &laid,
        format!("#!/bin/sh\nexec '{}' \"$@\"\n", real.display()),
    )
    .unwrap();
    std::fs::set_permissions(&laid, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::rename(&laid, &shim).unwrap();
}

/// `child`'s exit and its whole stdout, within the hang detector.
fn exit_and_stdout(mut child: Child) -> (std::process::ExitStatus, String) {
    let until = std::time::Instant::now() + HANG;
    let done = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if std::time::Instant::now() >= until {
            let _ = child.kill();
            panic!("hang detector: the stub never exited");
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    let mut stdout = String::new();
    std::io::Read::read_to_string(child.stdout.as_mut().unwrap(), &mut stdout).unwrap();
    (done, stdout)
}

/// AN UNATTENDED CALLER WAITS FOR ITS PROGRAM AND THEN RUNS IT: while a live pass's
/// `progress.json` has `trust` on its way, a `targo` started with no terminal (an agent's
/// tool call) says so on stderr and waits; the moment the real shim is renamed over the
/// stub, the command runs with its arguments and its stdout is the tool's alone.
#[test]
fn an_unattended_targo_waits_for_trust_and_then_runs() {
    if !atpkg::manager_enabled_with(atpkg::PKG_TRUST_ANCHORS) {
        return;
    }
    let fx = Fixture::new("wait");
    let out = fx.seed(false);
    assert!(out.status.success(), "{}", describe(&out));
    assert!(
        atpkg::stub::is_pending_stub(&fx.layout.bin_dir().join("targo")),
        "{}",
        describe(&out)
    );
    let (landed, heartbeat) = fake_pass(fx.layout.progress_file(), "trust");
    let (child, rx) = unattended_targo(&fx);
    // The stub says it waits before anything lands.
    let waiting = line_with(&rx, "trust is on its way");
    assert!(
        waiting.contains("targo starts as soon as it is installed"),
        "{waiting}"
    );
    land_targo(&fx);
    let (done, stdout) = exit_and_stdout(child);
    landed.store(true, std::sync::atomic::Ordering::SeqCst);
    heartbeat.join().unwrap();
    assert!(done.success(), "{done:?} stdout {stdout}");
    assert_eq!(
        stdout, "ran targo --version\n",
        "only the tool's own output on stdout"
    );
}

/// A STUB RUN BEFORE THE PASS BEGINS WAITS FOR IT. The window's first-run seed promises the
/// update it spawns next (`pass-owed.json`, the window its owner), so an unattended `targo`
/// run in the seconds before that update writes `progress.json` waits for trust instead of
/// exiting 127; the update begins, plans and retires the promise, trust lands, and the
/// command runs. NON-VACUITY: a typed seed promises nothing, and a promise whose owner is
/// gone keeps nobody waiting — the same stub answers 127 at once.
#[test]
fn an_unattended_targo_waits_for_the_pass_the_window_seed_promised() {
    if !atpkg::manager_enabled_with(atpkg::PKG_TRUST_ANCHORS) {
        return;
    }
    let fx = Fixture::new("promise");
    let out = fx.seed(false);
    assert!(out.status.success(), "{}", describe(&out));
    assert!(
        !fx.layout.pass_owed_file().exists(),
        "a typed seed promises nothing: {}",
        describe(&out)
    );
    let window_seed = |spawner: u32| {
        let progress = fx.layout.progress_file();
        let out = fx.seed_with(
            &[
                "--wait-lock",
                "5",
                "--progress-file",
                progress.to_str().unwrap(),
            ],
            &[(atpkg::cli::SPAWNER_PID_ENV, &spawner.to_string())],
        );
        assert!(out.status.success(), "{}", describe(&out));
        assert!(fx.layout.pass_owed_file().is_file(), "{}", describe(&out));
    };

    // The window that made the promise is gone: nothing is coming, 127 at once.
    let gone = Command::new("/usr/bin/true")
        .spawn()
        .map(|mut c| {
            let pid = c.id();
            let _ = c.wait();
            pid
        })
        .expect("a reaped child's pid");
    window_seed(gone);
    let (child, _rx) = unattended_targo(&fx);
    let (done, stdout) = exit_and_stdout(child);
    assert_eq!(done.code(), Some(127), "{done:?} stdout {stdout}");
    assert!(
        stdout.contains("nothing is installing right now"),
        "{stdout}"
    );

    // The window lives (this test process stands in for it): the stub waits for the pass,
    // whatever an earlier pass left in `progress.json` — here its failure of trust, which is
    // not this pass's. An update that returns before it planned (offline, nothing to do)
    // retires the promise, and the waiting stub answers at once: nothing is installing, 127.
    let ended = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        - 600;
    std::fs::write(
        fx.layout.progress_file(),
        format!(
            "{{\"v\":1,\"pid\":null,\"pass\":\"net\",\"heartbeat_unix\":{ended},\
             \"ended_unix\":{ended},\"queue\":[],\"programs\":{{\"trust\":\
             {{\"phase\":\"failed\",\"error\":\"an earlier pass's failure\"}}}}}}"
        ),
    )
    .unwrap();
    window_seed(std::process::id());
    let (child, rx) = unattended_targo(&fx);
    line_with(&rx, "trust is on its way");
    std::fs::remove_file(fx.layout.pass_owed_file()).unwrap();
    let (done, stdout) = exit_and_stdout(child);
    assert_eq!(done.code(), Some(127), "{done:?} stdout {stdout}");
    line_with(&rx, "nothing is installing right now");

    // And when the update does begin, plan and land trust, the waiting stub runs it.
    window_seed(std::process::id());
    let (child, rx) = unattended_targo(&fx);
    line_with(&rx, "starting your toolchain install");
    let waiting = line_with(&rx, "trust is on its way");
    assert!(
        waiting.contains("targo starts as soon as it is installed"),
        "{waiting}"
    );
    // The update begins and plans, and only then retires the promise.
    let (landed, heartbeat) = fake_pass(fx.layout.progress_file(), "trust");
    std::fs::remove_file(fx.layout.pass_owed_file()).unwrap();
    land_targo(&fx);
    let (done, stdout) = exit_and_stdout(child);
    landed.store(true, std::sync::atomic::Ordering::SeqCst);
    heartbeat.join().unwrap();
    assert!(done.success(), "{done:?} stdout {stdout}");
    assert_eq!(stdout, "ran targo --version\n");
}

/// A PROMISED PASS THAT PLANS WITHOUT TRUST ENDS THE WAIT AT ONCE: the update begins, plans
/// a set that leaves `trust` out (a name the signed index does not serve on this machine)
/// and retires the promise; the `targo` waiting on that promise answers 127 with the
/// honesty edge while the pass still runs, rather than waiting it out (the hang detector
/// fires if it does).
#[test]
fn a_waiting_targo_answers_once_the_promised_plan_leaves_trust_out() {
    if !atpkg::manager_enabled_with(atpkg::PKG_TRUST_ANCHORS) {
        return;
    }
    let fx = Fixture::new("left-out");
    let out = fx.seed(false);
    assert!(out.status.success(), "{}", describe(&out));
    let progress = fx.layout.progress_file();
    let out = fx.seed_with(
        &[
            "--wait-lock",
            "5",
            "--progress-file",
            progress.to_str().unwrap(),
        ],
        &[(atpkg::cli::SPAWNER_PID_ENV, &std::process::id().to_string())],
    );
    assert!(out.status.success(), "{}", describe(&out));
    assert!(fx.layout.pass_owed_file().is_file(), "{}", describe(&out));
    let (child, rx) = unattended_targo(&fx);
    line_with(&rx, "trust is on its way");
    // The update plans, without trust, and only then retires the promise.
    let (ended, heartbeat) = fake_pass(progress, "other");
    std::fs::remove_file(fx.layout.pass_owed_file()).unwrap();
    // The pass keeps running until the stub has answered.
    let (done, stdout) = exit_and_stdout(child);
    ended.store(true, std::sync::atomic::Ordering::SeqCst);
    heartbeat.join().unwrap();
    assert_eq!(done.code(), Some(127), "{done:?} stdout {stdout}");
    assert!(stdout.is_empty(), "{stdout}");
    line_with(&rx, "trust is no longer part of the default set");
}

/// THE UPDATE A SEED PROMISED ENDS THE PROMISE: a whole update that returns before any
/// plan — here its index cannot be resolved — retires the window seed's `pass-owed.json`,
/// so a stub waiting on it answers at once rather than at the promise's deadline.
#[test]
fn a_whole_update_ends_the_promise_it_was_spawned_on() {
    if !atpkg::manager_enabled_with(atpkg::PKG_TRUST_ANCHORS) {
        return;
    }
    let fx = Fixture::new("promise-end");
    let config = fx.root.path().join("config");
    std::fs::create_dir_all(config.join("aterm")).unwrap();
    // No vendor lane: every request this update could make goes to the empty registry.
    let vendors: Vec<String> = atpkg::vendor_direct::VENDORS
        .iter()
        .map(|spec| format!("{:?}", spec.program))
        .collect();
    std::fs::write(
        config.join("aterm/aterm.toml"),
        format!("[packages]\nexclude = [{}]\n", vendors.join(", ")),
    )
    .unwrap();
    let progress = fx.layout.progress_file();
    let flags = [
        "--wait-lock",
        "5",
        "--progress-file",
        progress.to_str().unwrap(),
    ];
    let me = std::process::id().to_string();
    let seed = fx.seed_with(&flags, &[(atpkg::cli::SPAWNER_PID_ENV, &me)]);
    assert!(seed.status.success(), "{}", describe(&seed));
    assert!(fx.layout.pass_owed_file().is_file(), "{}", describe(&seed));
    let mut child = Command::new(env!("CARGO_BIN_EXE_atpkg"))
        .arg("update")
        .args(flags)
        .env(atpkg::cli::SPAWNER_PID_ENV, &me)
        .env("HOME", &fx.home)
        .env("XDG_CONFIG_HOME", &config)
        .env(
            "ATPKG_REGISTRY",
            format!("dir:{}", fx.root.path().join("registry").display()),
        )
        .env_remove("RUSTUP_HOME")
        .env_remove("ATERM_CHILD")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run dev atpkg update");
    let until = std::time::Instant::now() + HANG;
    while child.try_wait().unwrap().is_none() {
        if std::time::Instant::now() >= until {
            let _ = child.kill();
            panic!("hang detector: `atpkg update` did not end");
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let update = child.wait_with_output().unwrap();
    assert!(
        !fx.layout.pass_owed_file().exists(),
        "the update's return retired the promise: {}",
        describe(&update)
    );
}

/// THE WINDOW'S SEED ASKS NO INDEX; A TYPED ONE STILL DOES. On an empty store whose config
/// wants no vendor program (every one excluded), the seed's one sentence resolved the signed
/// index — a discovery walk, the index quads and every manifest, dozens of requests under
/// the store lock — though the update the window runs right behind it
/// resolves the same index and says the same verdict. The window's seed (`--wait-lock`)
/// now makes no request at all. The typed seed still asks, and that is also what proves the
/// observable: every request the network fetcher makes goes through the proxy this test
/// listens as (`/usr/bin/curl` honours `https_proxy`), which counts the connection and
/// drops it, so nothing leaves the machine.
#[test]
fn a_window_seed_asks_no_index_and_a_typed_one_does() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    if !atpkg::manager_enabled_with(atpkg::PKG_TRUST_ANCHORS) {
        return; // A build that pins no root key adopts nothing and asks nothing.
    }
    let fx = Fixture::new("window");
    let config = fx.root.path().join("config");
    std::fs::create_dir_all(config.join("aterm")).unwrap();
    let vendors: Vec<String> = atpkg::vendor_direct::VENDORS
        .iter()
        .map(|spec| format!("{:?}", spec.program))
        .collect();
    std::fs::write(
        config.join("aterm/aterm.toml"),
        format!("[packages]\nexclude = [{}]\n", vendors.join(", ")),
    )
    .unwrap();

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let proxy = format!("http://{}", listener.local_addr().unwrap());
    let asked = std::sync::Arc::new(AtomicUsize::new(0));
    let stop = std::sync::Arc::new(AtomicBool::new(false));
    let counter = {
        let (asked, stop) = (std::sync::Arc::clone(&asked), std::sync::Arc::clone(&stop));
        std::thread::spawn(move || {
            while !stop.load(Ordering::SeqCst) {
                match listener.accept() {
                    // Counted, and dropped: the request fails at once, and goes nowhere.
                    Ok((stream, _)) => {
                        asked.fetch_add(1, Ordering::SeqCst);
                        drop(stream);
                    }
                    Err(_) => std::thread::sleep(std::time::Duration::from_millis(10)),
                }
            }
        })
    };
    let seed = |extra: &[&str]| -> Output {
        let mut child = Command::new(env!("CARGO_BIN_EXE_atpkg"))
            .arg("seed")
            .args(extra)
            .env("HOME", &fx.home)
            .env("XDG_CONFIG_HOME", &config)
            .env("https_proxy", &proxy)
            .env("HTTPS_PROXY", &proxy)
            .env("ALL_PROXY", &proxy)
            .env_remove("NO_PROXY")
            .env_remove("no_proxy")
            .env_remove("ATPKG_REGISTRY")
            .env_remove("RUSTUP_HOME")
            .env_remove("ATERM_CHILD")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("run dev atpkg seed");
        // Hang detector, not a latency budget: the typed seed's walk gives up within its
        // retries once the proxy drops it.
        let until = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while child.try_wait().unwrap().is_none() {
            if std::time::Instant::now() >= until {
                let _ = child.kill();
                panic!("hang detector: `atpkg seed {extra:?}` did not end");
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        child.wait_with_output().unwrap()
    };

    let window = seed(&["--wait-lock", "5"]);
    assert!(window.status.success(), "{}", describe(&window));
    assert!(
        fx.layout.adopted().is_file(),
        "the window's seed did its local work: {}",
        describe(&window)
    );
    assert_eq!(
        asked.load(Ordering::SeqCst),
        0,
        "the window's seed asks the network nothing: {}",
        describe(&window)
    );

    let typed = seed(&[]);
    stop.store(true, Ordering::SeqCst);
    counter.join().unwrap();
    assert!(typed.status.success(), "{}", describe(&typed));
    assert!(
        asked.load(Ordering::SeqCst) > 0,
        "the typed seed resolves the index through the counting proxy (the observable \
         works): {}",
        describe(&typed)
    );
}

/// `atpkg update` on the fixture's store and empty registry, with `args` after the verb and
/// `envs` set, to its end within the hang detector.
fn update_with(fx: &Fixture, args: &[&str], envs: &[(&str, &str)]) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_atpkg"))
        .arg("update")
        .args(args)
        .envs(envs.iter().copied())
        .env("HOME", &fx.home)
        .env("XDG_CONFIG_HOME", fx.root.path().join("config"))
        .env(
            "ATPKG_REGISTRY",
            format!("dir:{}", fx.root.path().join("registry").display()),
        )
        .env_remove("RUSTUP_HOME")
        .env_remove("ATERM_CHILD")
        .env_remove(atpkg::cli::SPAWNER_PID_ENV)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run dev atpkg update");
    let until = std::time::Instant::now() + HANG;
    while child.try_wait().unwrap().is_none() {
        if std::time::Instant::now() >= until {
            let _ = child.kill();
            panic!("hang detector: `atpkg update {args:?}` did not end");
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    child.wait_with_output().unwrap()
}

/// A config that wants no vendor program: every request an update makes goes to the
/// fixture's empty registry.
fn no_vendors(fx: &Fixture) {
    let config = fx.root.path().join("config");
    std::fs::create_dir_all(config.join("aterm")).unwrap();
    let vendors: Vec<String> = atpkg::vendor_direct::VENDORS
        .iter()
        .map(|spec| format!("{:?}", spec.program))
        .collect();
    std::fs::write(
        config.join("aterm/aterm.toml"),
        format!("[packages]\nexclude = [{}]\n", vendors.join(", ")),
    )
    .unwrap();
}

/// The window's seed, `spawner` the window: it promises the update the window runs next.
fn window_seed(fx: &Fixture, spawner: u32) {
    let progress = fx.layout.progress_file();
    let out = fx.seed_with(
        &[
            "--wait-lock",
            "5",
            "--progress-file",
            progress.to_str().unwrap(),
        ],
        &[(atpkg::cli::SPAWNER_PID_ENV, &spawner.to_string())],
    );
    assert!(out.status.success(), "{}", describe(&out));
    assert!(fx.layout.pass_owed_file().is_file(), "{}", describe(&out));
}

/// A TYPED UPDATE LEAVES A WINDOW SEED'S PROMISE TO THAT WINDOW'S UPDATE. `aterm pkg update`
/// typed between the window's seed and the update the window spawns returns without planning
/// the set (here its index cannot be resolved); the promise is not its own, nor its
/// spawner's, so it stands — a stub waiting on it keeps waiting for the window's update —
/// and the window's update, whose spawner holds it, retires it when it returns.
#[test]
fn a_typed_update_leaves_a_window_seeds_promise_to_that_window() {
    if !atpkg::manager_enabled_with(atpkg::PKG_TRUST_ANCHORS) {
        return;
    }
    let fx = Fixture::new("foreign-retire");
    no_vendors(&fx);
    let me = std::process::id().to_string();
    window_seed(&fx, std::process::id());
    let typed = update_with(&fx, &[], &[]);
    assert!(
        fx.layout.pass_owed_file().is_file(),
        "a typed update retired the window's promise: {}",
        describe(&typed)
    );
    let progress = fx.layout.progress_file();
    let window = update_with(
        &fx,
        &[
            "--wait-lock",
            "5",
            "--progress-file",
            progress.to_str().unwrap(),
        ],
        &[(atpkg::cli::SPAWNER_PID_ENV, &me)],
    );
    assert!(
        !fx.layout.pass_owed_file().exists(),
        "the window's update retired its promise: {}",
        describe(&window)
    );
}

/// A PROMISE ENDS WITH ITS OWNER, NOT WITH ITS PID. The promise records its owner's start
/// time; when the owner's pid names another process — a window that crashed, its pid taken —
/// a stub waiting on it answers 127 at once rather than at the deadline. NON-VACUITY: the
/// same promise naming the live owner as it started keeps the stub waiting.
#[test]
fn a_promise_whose_owner_pid_is_another_process_keeps_nobody_waiting() {
    if !atpkg::manager_enabled_with(atpkg::PKG_TRUST_ANCHORS) {
        return;
    }
    let fx = Fixture::new("promise-born");
    let out = fx.seed(false);
    assert!(out.status.success(), "{}", describe(&out));
    window_seed(&fx, std::process::id());
    let written = std::fs::read_to_string(fx.layout.pass_owed_file()).unwrap();
    let promise: atpkg::progress::PassPromise = aterm_json::from_str(&written).unwrap();
    let Some(born) = promise.born else {
        panic!("the seed records its owner's start time: {written}");
    };
    let (child, rx) = unattended_targo(&fx);
    line_with(&rx, "trust is on its way");
    // The owner's pid is now another process's.
    let reused = atpkg::progress::PassPromise {
        born: Some(born + 1),
        ..promise
    };
    std::fs::write(
        fx.layout.pass_owed_file(),
        aterm_json::to_string(&reused).unwrap(),
    )
    .unwrap();
    let (done, stdout) = exit_and_stdout(child);
    assert_eq!(done.code(), Some(127), "{done:?} stdout {stdout}");
    line_with(&rx, "nothing is installing right now");
}

/// A PENDING STUB SAYS WHETHER A PERSON ASKED. Its bump names the program either way —
/// every reader that orders work by it sees the same name — and a stub that has no terminal
/// on stdin and stderr (a script, an agent's tool call) marks its line unattended, so its
/// agent never jumps the toolchain's bytes; on a terminal (a pseudo-terminal here, which
/// `isatty` cannot tell from a person's) the line is the plain name a person's always was.
#[test]
fn a_pending_stub_says_whether_a_person_asked() {
    use std::os::fd::{FromRawFd as _, OwnedFd};
    if !atpkg::manager_enabled_with(atpkg::PKG_TRUST_ANCHORS) {
        return;
    }
    let fx = Fixture::new("asker");
    let out = fx.seed(false);
    assert!(out.status.success(), "{}", describe(&out));
    let bump = || std::fs::read_to_string(fx.layout.bump_file()).unwrap_or_default();
    let (child, _rx) = unattended_targo(&fx);
    let (done, _) = exit_and_stdout(child);
    assert_eq!(done.code(), Some(127));
    assert_eq!(bump(), "trust unattended\n", "a script asked");
    let (master, slave) = aterm_pty::open_pty_pair().expect("open_pty_pair");
    // SAFETY: both descriptors were just opened above and are owned by nothing else.
    let (master, slave) = unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
    let child = Command::new(fx.layout.bin_dir().join("targo"))
        .arg("--version")
        .env("HOME", &fx.home)
        .env("XDG_CONFIG_HOME", fx.root.path().join("config"))
        .env("PATH", "/usr/bin:/bin")
        .stdin(Stdio::from(slave.try_clone().unwrap()))
        .stdout(Stdio::piped())
        .stderr(Stdio::from(slave))
        .spawn()
        .expect("run the targo stub on a pseudo-terminal");
    let (done, _) = exit_and_stdout(child);
    drop(master);
    assert_eq!(done.code(), Some(127));
    assert_eq!(
        bump(),
        "trust unattended\ntrust\n",
        "a person at a terminal asked"
    );
}

/// A STUB THAT CANNOT READ THE OWNER'S START TIME STILL WAITS ON ITS LIVE PROMISE. An agent's
/// tool call run in a sandbox that denies process-info (`EPERM` from `proc_pidinfo`, while
/// `kill(pid, 0)` still answers) cannot tell the owner's start time; that says nothing about
/// the owner, so the promise rides on its pid as it did before start times were recorded,
/// and `targo` waits for trust and runs — never 127 "nothing is installing" while the window
/// is about to install it.
#[cfg(target_os = "macos")]
#[test]
fn a_sandboxed_stub_that_cannot_read_the_owners_start_time_still_waits() {
    if !atpkg::manager_enabled_with(atpkg::PKG_TRUST_ANCHORS) {
        return;
    }
    let fx = Fixture::new("promise-sandboxed");
    let out = fx.seed(false);
    assert!(out.status.success(), "{}", describe(&out));
    window_seed(&fx, std::process::id());
    let written = std::fs::read_to_string(fx.layout.pass_owed_file()).unwrap();
    let promise: atpkg::progress::PassPromise = aterm_json::from_str(&written).unwrap();
    assert!(
        promise.born.is_some(),
        "the seed records its owner's start time: {written}"
    );
    let (child, rx) = unattended_targo_under(
        &fx,
        &[
            "/usr/bin/sandbox-exec",
            "-p",
            "(version 1)(allow default)(deny process-info*)",
        ],
    );
    line_with(&rx, "trust is on its way");
    land_targo(&fx);
    let (done, stdout) = exit_and_stdout(child);
    assert!(done.success(), "{done:?} stdout {stdout}");
    assert_eq!(stdout, "ran targo --version\n");
}
