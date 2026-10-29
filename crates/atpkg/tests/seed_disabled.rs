// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Integration tests for `atpkg seed` install consent, shell hooks, and pending-stub waits.

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
        self.configure(declined);
        self.seed_with(&[], &[])
    }

    fn configure(&self, declined: bool) {
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
    }

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

    fn zshrc(&self) -> String {
        std::fs::read_to_string(self.home.join(".zshrc")).unwrap()
    }

    fn zsh_hook(&self) -> PathBuf {
        self.home.join(".aterm/shell.d/00-atpkg.zsh")
    }
}

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
    assert!(
        out.status.success(),
        "a declined seed is a quiet skip: {text}"
    );
    assert_eq!(
        fx.adoption_writes(),
        Vec::<PathBuf>::new(),
        "`auto_install = false` must stop seed before adoption, stubs, reroutes, shell \
         hooks or the rustup seam: {text}"
    );
    assert_eq!(fx.zshrc(), "", "no rc block either: {text}");

    needs_a_pinned_root();
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

fn needs_a_pinned_root() {
    if !atpkg::manager_enabled_with(atpkg::PKG_TRUST_ANCHORS) {
        aterm_uds::host::could_not_run(
            "this build pins no root key, so the package manager is off and adopts nothing",
        );
    }
}

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

#[test]
fn a_consenting_seed_puts_bin_on_path_and_answers_targo() {
    needs_a_pinned_root();
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
    let mut not_run = None;
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
            not_run = Some("/bin/zsh is absent, so no hooked shell's PATH or `targo` was read");
        }
        Err(e) => panic!("run zsh: {e}"),
    }

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

    std::fs::write(fx.home.join(".zshrc"), "").unwrap();
    let again = fx.seed(false);
    assert!(again.status.success(), "{}", describe(&again));
    assert_eq!(
        fx.zshrc(),
        "",
        "a deleted block is not re-added: {}",
        describe(&again)
    );
    if let Some(why) = not_run {
        aterm_uds::host::could_not_run(why);
    }
}

#[test]
fn the_store_and_its_hook_do_not_follow_xdg_data_home() {
    needs_a_pinned_root();
    let fx = Fixture::new("xdg-data");
    fx.configure(false);
    let data = fx.home.join("data");
    let data = data.to_str().expect("a UTF-8 scratch path");
    let with: &[(&str, &str)] = &[("XDG_DATA_HOME", data)];
    let mut hooks = Vec::new();
    for envs in [with, &[], with] {
        let out = fx.seed_with(&[], envs);
        let text = describe(&out);
        assert!(out.status.success(), "{envs:?}: {text}");
        assert!(fx.layout.adopted().is_file(), "{envs:?}: {text}");
        assert!(
            std::fs::symlink_metadata(std::path::Path::new(data).join("aterm")).is_err(),
            "{envs:?}: nothing lands under $XDG_DATA_HOME: {text}"
        );
        hooks.push(std::fs::read_to_string(fx.zsh_hook()).expect("the seed writes the hook"));
    }
    let bin = fx.layout.bin_dir();
    assert!(
        hooks[0].contains(bin.to_str().expect("a UTF-8 scratch path")),
        "the hook names the store's bin/ {}:\n{}",
        bin.display(),
        hooks[0]
    );
    assert!(
        hooks.iter().all(|hook| *hook == hooks[0]),
        "the hook stays put across environments: {hooks:#?}"
    );
}

fn fake_pass(
    progress: PathBuf,
    program: &'static str,
) -> (
    std::sync::Arc<std::sync::atomic::AtomicBool>,
    std::thread::JoinHandle<()>,
) {
    fake_pass_until_done(progress, program, None)
}

fn fake_pass_until_done(
    progress: PathBuf,
    program: &'static str,
    done: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
) -> (
    std::sync::Arc<std::sync::atomic::AtomicBool>,
    std::thread::JoinHandle<()>,
) {
    let write_progress = move || {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let row = if done
            .as_ref()
            .is_some_and(|d| d.load(std::sync::atomic::Ordering::SeqCst))
        {
            "{\"phase\":\"done\",\"bytes_done\":4,\"bytes_total\":4}"
        } else {
            "{\"phase\":\"download\",\"bytes_done\":1,\"bytes_total\":4}"
        };
        let body = format!(
            "{{\"v\":1,\"pid\":{},\"pass\":\"net\",\"heartbeat_unix\":{now},\
             \"overall\":{{\"programs_done\":0,\"programs_total\":1}},\
             \"queue\":[\"{program}\"],\"programs\":{{\"{program}\":{row}}}}}",
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

fn unattended_targo(fx: &Fixture) -> (Child, std::sync::mpsc::Receiver<String>) {
    unattended_targo_under(fx, &[])
}

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
    // SHELL is named, not inherited: the pending stub is a `#!/bin/sh` script, and macOS's
    // bash looks the account up (getpwuid) when SHELL is unset, which traps (SIGTRAP, "Unable
    // to get the unique pid") under a sandbox that denies process-info-pidinfo, before the stub
    // runs a line. A runner whose environment had no SHELL failed the sandboxed row by that.
    let mut child = command
        .arg("--version")
        .env("HOME", &fx.home)
        .env("XDG_CONFIG_HOME", fx.root.path().join("config"))
        .env("PATH", "/usr/bin:/bin")
        .env("SHELL", "/bin/sh")
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

const HANG: std::time::Duration = std::time::Duration::from_secs(60);

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

#[test]
fn an_unattended_targo_waits_for_trust_and_then_runs() {
    needs_a_pinned_root();
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

#[test]
fn an_unattended_targo_answers_once_trust_lands_without_it() {
    needs_a_pinned_root();
    let fx = Fixture::new("landed-without");
    let out = fx.seed(false);
    assert!(out.status.success(), "{}", describe(&out));
    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (ended, heartbeat) = fake_pass_until_done(
        fx.layout.progress_file(),
        "trust",
        Some(std::sync::Arc::clone(&done)),
    );
    let (child, rx) = unattended_targo(&fx);
    line_with(&rx, "trust is on its way");
    done.store(true, std::sync::atomic::Ordering::SeqCst);
    let (status, stdout) = exit_and_stdout(child);
    ended.store(true, std::sync::atomic::Ordering::SeqCst);
    heartbeat.join().unwrap();
    assert_eq!(status.code(), Some(127), "{status:?} stdout {stdout}");
    assert!(stdout.is_empty(), "{stdout}");
    line_with(&rx, "trust is installed, and it has no command named targo");
}

#[test]
fn an_unattended_targo_waits_for_the_pass_the_window_seed_promised() {
    needs_a_pinned_root();
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

    window_seed(std::process::id());
    let (child, rx) = unattended_targo(&fx);
    line_with(&rx, "starting your toolchain install");
    let waiting = line_with(&rx, "trust is on its way");
    assert!(
        waiting.contains("targo starts as soon as it is installed"),
        "{waiting}"
    );
    let (landed, heartbeat) = fake_pass(fx.layout.progress_file(), "trust");
    std::fs::remove_file(fx.layout.pass_owed_file()).unwrap();
    land_targo(&fx);
    let (done, stdout) = exit_and_stdout(child);
    landed.store(true, std::sync::atomic::Ordering::SeqCst);
    heartbeat.join().unwrap();
    assert!(done.success(), "{done:?} stdout {stdout}");
    assert_eq!(stdout, "ran targo --version\n");
}

#[test]
fn a_waiting_targo_answers_once_the_promised_plan_leaves_trust_out() {
    needs_a_pinned_root();
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
    let (ended, heartbeat) = fake_pass(progress, "other");
    std::fs::remove_file(fx.layout.pass_owed_file()).unwrap();
    let (done, stdout) = exit_and_stdout(child);
    ended.store(true, std::sync::atomic::Ordering::SeqCst);
    heartbeat.join().unwrap();
    assert_eq!(done.code(), Some(127), "{done:?} stdout {stdout}");
    assert!(stdout.is_empty(), "{stdout}");
    line_with(&rx, "trust is no longer part of the default set");
}

#[test]
fn a_whole_update_ends_the_promise_it_was_spawned_on() {
    needs_a_pinned_root();
    let fx = Fixture::new("promise-end");
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

#[test]
fn a_window_seed_asks_no_index_and_a_typed_one_does() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    needs_a_pinned_root();
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
            .env(aterm_types::domain::ENV_DEV_FAST_TIMERS, "1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("run dev atpkg seed");
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

#[test]
fn a_typed_update_leaves_a_window_seeds_promise_to_that_window() {
    needs_a_pinned_root();
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

#[test]
fn a_promise_whose_owner_pid_is_another_process_keeps_nobody_waiting() {
    needs_a_pinned_root();
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

#[test]
fn a_pending_stub_says_whether_a_person_asked() {
    use std::os::fd::{FromRawFd as _, OwnedFd};
    needs_a_pinned_root();
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

/// A stub run in a sandbox that denies `process-info-pidinfo` (`EPERM` from `proc_pidinfo`, while
/// `kill(pid, 0)` still answers) falls back to checking the owner's pid and waits for `trust` to
/// land.
#[cfg(target_os = "macos")]
#[test]
fn a_sandboxed_stub_that_cannot_read_the_owners_start_time_still_waits() {
    needs_a_pinned_root();
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
            "(version 1)(allow default)(deny process-info-pidinfo)",
        ],
    );
    line_with(&rx, "trust is on its way");
    land_targo(&fx);
    let (done, stdout) = exit_and_stdout(child);
    assert!(done.success(), "{done:?} stdout {stdout}");
    assert_eq!(stdout, "ran targo --version\n");
}

/// A pass whose stderr the window copies into its aterm.log (the package lanes set
/// [`atpkg::notice::STDERR_IS_HOST_LOG_ENV`]) prints an aterm.toml that is not TOML by its
/// position, never the line that did not parse: the line is the person's config (DESIGN §3.6). A
/// seed a person types still prints the line.
#[test]
fn a_hosts_pass_says_an_unparsed_aterm_toml_by_its_position() {
    let fx = Fixture::new("host-read-stderr");
    let config = fx.root.path().join("config");
    std::fs::create_dir_all(config.join("aterm")).unwrap();
    std::fs::write(
        config.join("aterm/aterm.toml"),
        "[packages]\nauto_install = false\nfont_family = \"QZP7E7L1\n",
    )
    .unwrap();
    let seed = |copied: bool| {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_atpkg"));
        cmd.arg("seed")
            .env("HOME", &fx.home)
            .env("XDG_CONFIG_HOME", &config)
            .env(
                "ATPKG_REGISTRY",
                format!("dir:{}", fx.root.path().join("registry").display()),
            )
            .env_remove("RUSTUP_HOME")
            .env_remove("ATERM_CHILD")
            .env_remove(atpkg::notice::STDERR_IS_HOST_LOG_ENV)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if copied {
            cmd.env(atpkg::notice::STDERR_IS_HOST_LOG_ENV, "1");
        }
        let out = cmd.output().expect("run dev atpkg seed");
        String::from_utf8_lossy(&out.stderr).into_owned()
    };
    let copied = seed(true);
    assert!(
        copied.lines().any(|line| line
            .starts_with("atpkg: aterm.toml is not valid TOML (TOML error at line 3, column ")),
        "the host's copy names the error by its position: {} lines",
        copied.lines().count()
    );
    assert!(
        !copied.contains("QZP7E7L"),
        "a pass the host's log reads printed aterm.toml's line"
    );
    // CONTROL: the seed a person types prints the line that did not parse.
    assert!(
        seed(false).contains("QZP7E7L1"),
        "a typed seed says the line whole"
    );
}
