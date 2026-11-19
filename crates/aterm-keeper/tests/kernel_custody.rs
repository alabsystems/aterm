// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Real-kernel PTY custody, orphan recovery, and shared-memory journal tests for `aterm-keeper`.

#![cfg(target_vendor = "apple")]
#![allow(
    clippy::disallowed_types,
    reason = "test code: a stand-in child may inherit descriptors"
)]

use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use aterm_keeper::client::{KeeperClient, KeeperLink, LinkOp};
use aterm_keeper::code::ExeImage;
use aterm_keeper::identity::Identity;
use aterm_keeper::server::{Relauncher, ServeConfig, Server};
use aterm_keeper::wire::{Birth, Frame, MarkerRef, MasterHeader};
use aterm_spec::wait::{MUST_HAPPEN, until_while_working, within};
use aterm_uds::deadline::{FIRST_EXEC_HANG, KeptUntilRan};
use aterm_uds::exitwatch::ExitWatch;

const ROLE: &str = "ATERM_KEEPER_TEST_ROLE";
const SOCK: &str = "ATERM_KEEPER_TEST_SOCK";
const MARK: &str = "ATERM_KEEPER_TEST_MARK";
const DROP: &str = "ATERM_KEEPER_TEST_DROP_CUSTODY";
const TALK: &str = "ATERM_KEEPER_TEST_TALK";
const LOGS: &str = "ATERM_KEEPER_TEST_LOGS";
/// The `--job` label a `keeper-cli` role passes `aterm keeper serve`, if any.
const CLI_JOB: &str = "ATERM_KEEPER_TEST_CLI_JOB";
/// The version a restarting keeper runs as, and its copy's bundle names: a restart goes only for a
/// build at least as new (`ExeImage::look`).
const KEEPER_VERSION: &str = "0.1.0";
const HANG: Duration = MUST_HAPPEN;

unsafe extern "C" {
    fn fcntl(fd: i32, cmd: i32, ...) -> i32;
    fn setsid() -> i32;
    fn ioctl(fd: i32, req: u64, ...) -> i32;
    fn kill(pid: i32, sig: i32) -> i32;
}

const F_DUPFD: i32 = 0;
const F_SETFD: i32 = 2;
const TIOCSCTTY: u64 = 0x2000_7461;

fn role() -> Option<String> {
    std::env::var(ROLE).ok()
}

fn sigkill(pid: u32) {
    // SAFETY: sends `SIGKILL` to a child process spawned by this test.
    unsafe { kill(pid as i32, 9) };
}

fn keeper_config() -> ServeConfig {
    let socket = PathBuf::from(std::env::var(SOCK).expect("socket"));
    let mark = std::env::var(MARK).expect("marker");
    ServeConfig {
        socket,
        identity: Identity::same_uid(),
        relaunch: Relauncher::TestCommand(vec![
            "/bin/sh".into(),
            "-c".into(),
            format!("echo relaunch >> '{mark}'"),
        ]),
        version: "test".into(),
        test_drop_custody: std::env::var_os(DROP).is_some(),
    }
}

fn keeper_role() -> ! {
    let mut server = Server::bind(keeper_config()).expect("bind");
    report("ready");
    let _ = server.run();
    std::process::exit(1)
}

/// A keeper that honours RESTART_WHEN_SAFE for the file at its own path (the test runs it from
/// a copy it then replaces), and exits as `aterm keeper serve` does once it restarts: 0. It
/// prints `ready` on stdout, the line that proves its exec ran from the copy.
fn restarting_keeper_role() -> ! {
    let exe = std::env::current_exe().expect("its path");
    let image = ExeImage::at(&exe).expect("the file it runs from");
    let mut server = Server::bind(ServeConfig {
        version: KEEPER_VERSION.into(),
        ..keeper_config()
    })
    .expect("bind");
    server.restart_for(image);
    report("ready");
    println!("ready");
    let ran = server.run();
    eprintln!("keeper role: run returned {ran:?}");
    std::process::exit(i32::from(ran.is_err()))
}

/// `aterm keeper serve` itself ([`aterm_keeper::cli::main_entry`]) on the test's socket at
/// [`KEEPER_VERSION`], passed `--job` with the label [`CLI_JOB`] names, if any: what decides
/// whether it honours RESTART_WHEN_SAFE. It prints `ready` on stdout first, the line that proves
/// its exec ran from the copy, and exits as `serve` does.
fn cli_keeper_role() -> ! {
    println!("ready");
    let sock = std::env::var(SOCK).expect("socket");
    let mut args: Vec<std::ffi::OsString> = ["serve", "--sock", &sock, "--identity", "uid"]
        .into_iter()
        .chain(["--no-relaunch"])
        .map(Into::into)
        .collect();
    if let Ok(job) = std::env::var(CLI_JOB) {
        args.extend(["--job".into(), job.into()]);
    }
    let code = aterm_keeper::cli::main_entry(args, KEEPER_VERSION);
    std::process::exit(i32::from(code != std::process::ExitCode::SUCCESS))
}

/// A keeper the test parks with a `pause` note and lets go with a `resume` one: between two
/// turns, or with `in_turn` inside one, after its poll returned and before it reads anything
/// (`Server::step_parked`), so what the test does meanwhile lands between that poll and the
/// turn's sweep of its exit watches.
fn stepped_keeper_role(in_turn: bool) -> ! {
    let talk = PathBuf::from(std::env::var(TALK).expect("talk"));
    let note = |ext: &str| talk.with_extension(ext);
    let park = || {
        std::fs::remove_file(note("pause")).expect("take the pause");
        std::fs::write(note("paused"), b"paused").expect("say parked");
        let resumed = within(HANG * 5, Duration::from_millis(5), || {
            note("resume").exists().then_some(())
        });
        assert!(resumed.is_some(), "never resumed");
        std::fs::remove_file(note("resume")).expect("take the resume");
    };
    let mut server = Server::bind(keeper_config()).expect("bind");
    report("ready");
    loop {
        let parked = note("pause").exists();
        let turned = if parked && in_turn {
            server.step_parked(20, park)
        } else {
            if parked {
                park();
            }
            server.step(20)
        };
        if turned.is_err() {
            std::process::exit(1)
        }
        if parked {
            std::fs::write(note("turned"), b"turned").expect("say turned");
        }
    }
}

fn pty_with_leader() -> (OwnedFd, Child) {
    use aterm_uds::spawnfd::InheritNothing as _;
    let (master, slave) = aterm_pty::open_pty_pair().expect("open_pty_pair");
    // SAFETY: `master` and `slave` are two fresh descriptors from `open_pty_pair`; this
    // function alone owns them.
    let (master, slave) = unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
    let stdio = || Stdio::from(slave.try_clone().expect("dup the slave"));
    let mut cmd = Command::new("/bin/sleep");
    cmd.inherit_nothing()
        .arg("600")
        .stdin(stdio())
        .stdout(stdio())
        .stderr(stdio());
    // SAFETY: the hook makes only async-signal-safe calls between fork and exec: `setsid`, and
    // `ioctl` on the child's own stdin, which is the slave by then.
    unsafe {
        use std::os::unix::process::CommandExt as _;
        cmd.pre_exec(|| {
            if setsid() < 0 || ioctl(0, TIOCSCTTY, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let leader = cmd.spawn().expect("the session leader");
    drop(slave);
    (master, leader)
}

fn rdev_of(fd: &OwnedFd) -> u64 {
    use std::os::unix::fs::MetadataExt as _;
    let dup = fd.try_clone().expect("dup");
    std::fs::File::from(dup).metadata().expect("fstat").rdev()
}

fn window_role(holder: bool) -> ! {
    let socket = PathBuf::from(std::env::var(SOCK).expect("socket"));
    let (master, leader) = pty_with_leader();
    let rdev = rdev_of(&master);
    let birth = born(leader.id());
    let client = KeeperClient::connect(&socket, &Identity::same_uid(), HANG).expect("connect");
    let offers = client.hello_app("test").expect("hello");
    assert!(offers.is_empty(), "a fresh window is offered nothing");
    let dup = master.try_clone().expect("dup for the keeper");
    client
        .register(
            &dup,
            MasterHeader {
                rdev,
                shell_pid: leader.id(),
                shell_birth: birth,
                local_id: 1,
            },
            b"sid=s-test".to_vec(),
        )
        .expect("register");
    drop(dup);
    let mut line = format!("leader={} rdev={rdev}", leader.id());
    if holder {
        // SAFETY: `F_DUPFD` duplicates owned `master` descriptor.
        let raw = unsafe { fcntl(master.as_raw_fd(), F_DUPFD, 200) };
        assert!(
            raw >= 200,
            "F_DUPFD at 200: {}",
            std::io::Error::last_os_error()
        );
        let held = aterm_uds::spawnfd::transparent("/bin/sleep")
            .arg("600")
            .spawn()
            .expect("holder");
        // SAFETY: `raw` is the local duplicate descriptor created above.
        drop(unsafe { OwnedFd::from_raw_fd(raw) });
        line.push_str(&format!(" holder={}", held.id()));
        std::mem::forget(held);
    }
    report(&line);
    std::mem::forget(leader);
    match await_go().as_str() {
        "bye" => client
            .send(&Frame::Bye { keep: Vec::new() }, None)
            .expect("bye"),
        "bye-keep" => client
            .send(&Frame::Bye { keep: vec![rdev] }, None)
            .expect("bye"),
        "poke" => {
            client.send(&Frame::RestartWhenSafe, None).expect("a frame");
            note("poked");
            let started = Instant::now();
            loop {
                assert!(started.elapsed() < HANG * 5, "never killed");
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        _ => {}
    }
    drop(master);
    std::process::exit(0)
}

fn marked_window_role(named: bool) -> ! {
    use std::os::unix::fs::OpenOptionsExt as _;
    let socket = PathBuf::from(std::env::var(SOCK).expect("socket"));
    let logs = PathBuf::from(std::env::var(LOGS).expect("logs"));
    let nanos: u64 = 1_790_000_000_000_000_007;
    let name = format!("crash-marker-{}-{nanos}-other.log", std::process::id());
    let pending = logs.join(format!(".{name}.pending"));
    let marker = logs.join(&name);
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&pending)
        .expect("the marker");
    #[expect(
        clippy::disallowed_methods,
        reason = "the owner lock, held for the role process's life: its exit releases it"
    )]
    file.try_lock().expect("the owner lock");
    std::fs::rename(&pending, &marker).expect("the marker in place");
    let (master, leader) = pty_with_leader();
    let rdev = rdev_of(&master);
    let birth = born(leader.id());
    let client = KeeperClient::connect(&socket, &Identity::same_uid(), HANG).expect("connect");
    let named_ref = named.then(|| {
        use std::os::unix::ffi::OsStrExt as _;
        MarkerRef {
            nanos,
            dir: logs.as_os_str().as_bytes().to_vec(),
        }
    });
    let offers = client
        .hello_app_marked("test", named_ref, false)
        .expect("hello");
    assert!(offers.is_empty());
    let dup = master.try_clone().expect("dup for the keeper");
    client
        .register(
            &dup,
            MasterHeader {
                rdev,
                shell_pid: leader.id(),
                shell_birth: birth,
                local_id: 1,
            },
            b"sid=s-test".to_vec(),
        )
        .expect("register");
    drop(dup);
    report(&format!("leader={} rdev={rdev}", leader.id()));
    std::mem::forget(leader);
    await_go();
    std::fs::remove_file(&marker).expect("the exit path unlinks the marker");
    drop(master);
    std::process::exit(0)
}

fn next_window_role(bye: bool, adopt: bool) -> ! {
    let socket = PathBuf::from(std::env::var(SOCK).expect("socket"));
    let client = KeeperClient::connect(&socket, &Identity::same_uid(), HANG).expect("connect");
    let offers = client.hello_app("test").expect("hello");
    let rdevs: Vec<String> = offers.iter().map(|o| o.header.rdev.to_string()).collect();
    for o in &offers {
        assert_eq!(
            rdev_of(&o.master),
            o.header.rdev,
            "the offered descriptor is that master"
        );
        assert_eq!(o.tag, b"sid=s-test", "the tag travels opaque");
    }
    report(&format!(
        "offers={} rdevs={} kept={}",
        offers.len(),
        rdevs.join(","),
        offers.iter().filter(|o| o.kept).count()
    ));
    await_go();
    for (i, o) in offers.iter().enumerate() {
        client
            .register(&o.master, o.header, o.tag.clone())
            .expect("claim");
        if adopt && i == 0 {
            client.send(&Frame::Adopt, None).expect("adopt");
        }
    }
    if bye {
        client
            .send(&Frame::Bye { keep: Vec::new() }, None)
            .expect("bye");
    }
    std::process::exit(0)
}

fn old_window_role() -> ! {
    use aterm_keeper::wire::{CAP_SENDS_BYE, PROTO, PeerClass};
    let socket = PathBuf::from(std::env::var(SOCK).expect("socket"));
    let client = KeeperClient::connect(&socket, &Identity::same_uid(), HANG).expect("connect");
    let hello = Frame::Hello {
        proto: PROTO,
        class: PeerClass::App,
        caps: CAP_SENDS_BYE,
        build: "pre-journal".into(),
        marker: None,
    };
    client.send(&hello, None).expect("hello");
    let next = || client.recv().expect("a frame").expect("not EOF");
    let n = match next().frame {
        Frame::Welcome { offers, .. } => offers,
        other => panic!("expected WELCOME, got {other:?}"),
    };
    let mut offers = Vec::new();
    let mut trailers = 0;
    for _ in 0..n {
        match next() {
            aterm_keeper::frameio::Incoming {
                frame:
                    Frame::Offer {
                        header,
                        tag,
                        journal,
                        ..
                    },
                fd: Some(master),
            } => {
                trailers += usize::from(journal.is_some());
                offers.push((master, header, tag));
            }
            other => panic!("expected an OFFER with its master, got {:?}", other.frame),
        }
    }
    client
        .send(
            &Frame::Meta {
                rdev: 0,
                meta: Vec::new(),
            },
            None,
        )
        .expect("the probe");
    let mut strays = Vec::new();
    loop {
        match next().frame {
            Frame::Refused { rdev: 0, .. } => break,
            other => strays.push(format!("{other:?}")),
        }
    }
    let rdevs: Vec<String> = offers.iter().map(|(_, h, _)| h.rdev.to_string()).collect();
    let mut line = format!(
        "offers={} trailers={trailers} strays={} rdevs={}",
        offers.len(),
        strays.len(),
        rdevs.join(",")
    );
    if !strays.is_empty() {
        line.push_str(&format!(" stray=[{}]", strays.join(" | ")));
    }
    report(&line);
    await_said("claim");
    for (master, header, tag) in &offers {
        client
            .register(master, *header, tag.clone())
            .expect("claim");
    }
    report("claimed");
    await_said("quit");
    client
        .send(&Frame::Bye { keep: Vec::new() }, None)
        .expect("bye");
    std::process::exit(0)
}

fn await_said(what: &str) {
    let go = PathBuf::from(std::env::var(TALK).expect("talk")).with_extension("go");
    within(HANG, Duration::from_millis(5), || {
        (std::fs::read_to_string(&go).ok()? == what).then_some(())
    })
    .unwrap_or_else(|| panic!("never told {what:?}"));
}

fn unplaced_window_role() -> ! {
    let socket = PathBuf::from(std::env::var(SOCK).expect("socket"));
    let client = KeeperClient::connect(&socket, &Identity::same_uid(), HANG).expect("connect");
    let offers = client.hello_app("test").expect("hello");
    let rdevs: Vec<u64> = offers.iter().map(|o| o.header.rdev).collect();
    drop(offers);
    let link = KeeperLink::start_on(socket, Identity::same_uid(), "test".into(), Some(client))
        .expect("link");
    for rdev in &rdevs {
        assert!(link.enqueue(LinkOp::Release { rdev: *rdev }), "queued");
    }
    assert!(
        link.send_and_wait(LinkOp::Bye { keep: Vec::new() }, HANG),
        "the BYE is written"
    );
    report(&format!("released={}", rdevs.len()));
    std::process::exit(0)
}

fn outgoing_window_role() -> ! {
    let socket = PathBuf::from(std::env::var(SOCK).expect("socket"));
    let talk = std::env::var(TALK).expect("talk");
    let (master, leader) = pty_with_leader();
    let rdev = rdev_of(&master);
    let birth = born(leader.id());
    let client = KeeperClient::connect(&socket, &Identity::same_uid(), HANG).expect("connect");
    assert!(client.hello_app("test").expect("hello").is_empty());
    let dup = master.try_clone().expect("dup for the keeper");
    client
        .register(
            &dup,
            MasterHeader {
                rdev,
                shell_pid: leader.id(),
                shell_birth: birth,
                local_id: 1,
            },
            b"sid=s-test".to_vec(),
        )
        .expect("register");
    drop(dup);
    // SAFETY: `F_DUPFD` duplicates owned `master` descriptor.
    let raw = unsafe { fcntl(master.as_raw_fd(), F_DUPFD, 200) };
    assert!(
        raw >= 200,
        "F_DUPFD at 200: {}",
        std::io::Error::last_os_error()
    );
    let mut cmd = Command::new(std::env::current_exe().expect("test binary"));
    cmd.args(std::env::args().skip(1))
        .env(ROLE, "quiet-successor")
        .env(TALK, format!("{talk}-succ"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // SAFETY: `pre_exec` runs one async-signal-safe `fcntl` between fork and exec, on `raw`,
    // a descriptor the child holds from the fork.
    unsafe {
        use std::os::unix::process::CommandExt as _;
        cmd.pre_exec(move || {
            if fcntl(raw, F_SETFD, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let successor = cmd.spawn().expect("the successor");
    // SAFETY: `raw` is the local duplicate descriptor created above.
    drop(unsafe { OwnedFd::from_raw_fd(raw) });
    client
        .send(
            &Frame::Pending {
                pid: successor.id(),
            },
            None,
        )
        .expect("pending");
    report(&format!(
        "leader={} rdev={rdev} successor={}",
        leader.id(),
        successor.id()
    ));
    std::mem::forget(leader);
    std::mem::forget(successor);
    await_go();
    drop(master);
    std::process::exit(0)
}

fn quiet_successor_role() -> ! {
    await_go();
    std::process::exit(0)
}

fn report(line: &str) {
    let talk = PathBuf::from(std::env::var(TALK).expect("talk"));
    let tmp = talk.with_extension("tmp");
    std::fs::write(&tmp, format!("{line}\n")).expect("report");
    std::fs::rename(&tmp, talk.with_extension("out")).expect("publish the report");
}

fn note(what: &str) {
    let talk = PathBuf::from(std::env::var(TALK).expect("talk"));
    std::fs::write(talk.with_extension(what), what).expect("note");
}

fn await_go() -> String {
    let go = PathBuf::from(std::env::var(TALK).expect("talk")).with_extension("go");
    within(HANG * 5, Duration::from_millis(5), || {
        std::fs::read_to_string(&go).ok()
    })
    .expect("never told to go")
}

fn dispatch_role() {
    match role().as_deref() {
        Some("keeper") => keeper_role(),
        Some("keeper-restarting") => restarting_keeper_role(),
        Some("keeper-cli") => cli_keeper_role(),
        Some("keeper-stepped") => stepped_keeper_role(false),
        Some("keeper-parked-in-turn") => stepped_keeper_role(true),
        Some("window") => window_role(false),
        Some("window-holder") => window_role(true),
        Some(
            role @ ("next-window"
            | "next-window-no-bye"
            | "next-window-adopt"
            | "next-window-adopt-no-bye"),
        ) => {
            next_window_role(!role.ends_with("-no-bye"), role.contains("-adopt"));
        }
        Some("marked-window") => marked_window_role(true),
        Some("unmarked-window") => marked_window_role(false),
        Some("unplaced-window") => unplaced_window_role(),
        Some("outgoing-window") => outgoing_window_role(),
        Some("quiet-successor") => quiet_successor_role(),
        Some("journal-window") => journal_window_role(Journaled::Region),
        Some("journal-window-bare") => journal_window_role(Journaled::Nothing),
        Some("journal-window-writable") => journal_window_role(Journaled::Writable),
        Some("journal-next-window") => journal_next_window_role(),
        Some("old-window") => old_window_role(),
        _ => {}
    }
}

fn born(pid: u32) -> Birth {
    aterm_keeper::server::born(pid).expect("a live process has a birth record")
}

struct Scratch {
    dir: PathBuf,
    /// Guarded `.ubd` `.noindex` directory; dropped after `Scratch::drop` kills roles and drains
    /// `self.dir`.
    _scratch: aterm_tempfile::TempDir,
    /// Each started by `spawnfd`: it holds its stdio and nothing else of this
    /// process, not another test's pipe for as long as its role lives.
    kids: Vec<aterm_uds::spawnfd::Child>,
    pids: Vec<u32>,
}

impl Drop for Scratch {
    fn drop(&mut self) {
        for k in &mut self.kids {
            let _ = k.kill();
            let _ = k.wait();
        }
        for p in &self.pids {
            if aterm_uds::process::pid_alive(*p) {
                sigkill(*p);
            }
        }
        for _ in 0..4 {
            match std::fs::remove_dir_all(&self.dir) {
                Err(e) if e.kind() == std::io::ErrorKind::DirectoryNotEmpty => {}
                _ => break,
            }
        }
    }
}

impl Scratch {
    fn new(name: &str) -> Self {
        let mut end = name.len().min(12);
        while end > 0 && !name.is_char_boundary(end) {
            end -= 1;
        }
        let short = &name[..end];
        let scratch = aterm_tempfile::Builder::new()
            .prefix(&format!("akk-{short}-"))
            .suffix(".noindex")
            .tempdir_in("/tmp")
            .expect("scratch");
        let dir = scratch.path().to_path_buf();
        std::fs::create_dir_all(dir.join("logs")).expect("scratch");
        Scratch {
            dir,
            _scratch: scratch,
            kids: Vec::new(),
            pids: Vec::new(),
        }
    }
    fn sock(&self) -> PathBuf {
        self.dir.join("k.sock")
    }
    fn mark(&self) -> PathBuf {
        self.dir.join("relaunches")
    }
    fn spawn(&mut self, test: &str, role: &str, extra: &[(&str, &str)]) -> usize {
        let exe = std::env::current_exe().expect("test binary");
        self.spawn_from(&exe, test, role, extra, false)
    }
    /// [`Self::spawn`] from the executable at `exe`; with `proof`, its stdout is a pipe for
    /// [`aterm_uds::deadline::prints`] and its stderr goes to `kid-<n>.log`.
    fn spawn_from(
        &mut self,
        exe: &Path,
        test: &str,
        role: &str,
        extra: &[(&str, &str)],
        proof: bool,
    ) -> usize {
        use aterm_uds::spawnfd::Stdio as Io;
        let kid = self.kids.len();
        let mut cmd = aterm_uds::spawnfd::command(exe);
        cmd.arg(test)
            .arg("--exact")
            .arg("--nocapture")
            .arg("--test-threads=1")
            .env(ROLE, role)
            .env(SOCK, self.sock())
            .env(MARK, self.mark())
            .env(TALK, self.dir.join(format!("kid-{kid}")))
            .env(LOGS, self.dir.join("logs"))
            .stdin(Io::null());
        if proof {
            let log = std::fs::File::create(self.log_of(kid)).expect("a log");
            cmd.stdout(Io::piped()).stderr(log);
        } else {
            cmd.stdout(Io::null()).stderr(Io::null());
        }
        for (k, v) in extra {
            cmd.env(k, v);
        }
        self.kids.push(cmd.spawn().expect("spawn a role"));
        kid
    }
    fn log_of(&self, kid: usize) -> PathBuf {
        self.dir.join(format!("kid-{kid}.log"))
    }
    fn line(&mut self, kid: usize, prefix: &str) -> String {
        let out = self.dir.join(format!("kid-{kid}.out"));
        let text = within(HANG, Duration::from_millis(5), || {
            if let Ok(text) = std::fs::read_to_string(&out) {
                return Some(text);
            }
            if let Ok(Some(status)) = self.kids[kid].try_wait() {
                return Some(std::fs::read_to_string(&out).unwrap_or_else(|_| {
                    panic!("the child ended ({status}) before reporting {prefix:?}")
                }));
            }
            None
        })
        .unwrap_or_else(|| panic!("no report {prefix:?}"));
        let line = text.trim().to_string();
        assert!(
            line.starts_with(prefix),
            "{prefix:?} expected, got {line:?}"
        );
        line
    }
    fn say(&mut self, kid: usize, what: &str) {
        self.tell(&format!("kid-{kid}"), what);
    }
    fn tell(&self, talk: &str, what: &str) {
        let tmp = self.dir.join(format!("{talk}.say"));
        std::fs::write(&tmp, what).expect("say");
        std::fs::rename(&tmp, self.dir.join(format!("{talk}.go"))).expect("go");
    }
    fn pause(&mut self, kid: usize) {
        std::fs::write(self.dir.join(format!("kid-{kid}.pause")), b"pause").expect("pause");
        self.await_note(kid, "paused");
    }
    fn resume(&mut self, kid: usize) {
        std::fs::write(self.dir.join(format!("kid-{kid}.resume")), b"resume").expect("resume");
        self.await_note(kid, "turned");
    }
    fn await_note(&mut self, kid: usize, what: &str) {
        let note = self.dir.join(format!("kid-{kid}.{what}"));
        let seen = within(HANG, Duration::from_millis(5), || {
            if note.exists() {
                return Some(());
            }
            if let Ok(Some(status)) = self.kids[kid].try_wait() {
                panic!("role {kid} ended ({status}) before it {what}");
            }
            None
        });
        assert!(seen.is_some(), "role {kid} never {what}");
        std::fs::remove_file(&note).expect("take the note");
    }
    fn keeper(&mut self, test: &str, drop_custody: bool) -> usize {
        let extra: &[(&str, &str)] = if drop_custody { &[(DROP, "1")] } else { &[] };
        let k = self.spawn(test, "keeper", extra);
        self.line(k, "ready");
        k
    }
    fn status(&self) -> String {
        KeeperClient::connect(&self.sock(), &Identity::same_uid(), HANG)
            .and_then(|c| c.status("test"))
            .expect("status")
    }
    fn await_status(&self, what: &str, pred: impl Fn(&str) -> bool) -> String {
        let mut last = String::new();
        let hit = within(HANG, Duration::ZERO, || {
            last = self.status();
            pred(&last).then(|| last.clone())
        });
        hit.unwrap_or_else(|| panic!("never {what}:\n{last}"))
    }
}

fn keeper_regions(s: &Scratch) -> Vec<u64> {
    let pid = u32::try_from(field(&s.status(), "pid")).expect("a pid");
    aterm_uds::holders::pshm_sizes_of(pid).expect("the keeper's descriptor table")
}

const REGION: u64 = aterm_restore::REGION_LEN as u64;

fn field(line: &str, key: &str) -> u64 {
    line.split_whitespace()
        .find_map(|kv| kv.strip_prefix(&format!("{key}=")))
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(|| panic!("{key} in {line:?}"))
}

fn await_exit(watch: &ExitWatch) -> i32 {
    within(HANG, Duration::ZERO, || watch.exit_status())
        .unwrap_or_else(|| panic!("pid {} never exited", watch.pid()))
}

fn relaunch_lines(s: &Scratch) -> usize {
    std::fs::read_to_string(s.mark())
        .map(|t| t.lines().count())
        .unwrap_or(0)
}

const T_CRASH: &str = "a_killed_window_s_shell_lives_and_is_offered_once";

#[test]
fn a_killed_window_s_shell_lives_and_is_offered_once() {
    dispatch_role();
    let mut s = Scratch::new("crash");
    let keeper = s.keeper(T_CRASH, false);
    let keeper_pid = s.kids[keeper].id();
    let w = s.spawn(T_CRASH, "window", &[]);
    let reg = s.line(w, "leader=");
    let (leader, rdev) = (field(&reg, "leader") as u32, field(&reg, "rdev"));
    s.pids.push(leader);
    let leader_watch = ExitWatch::watch(leader).expect("watch the leader");
    s.await_status("registered", |t| t.contains("claimed=1"));
    assert_eq!(
        aterm_uds::holders::process_holds_rdev(keeper_pid, rdev),
        Some(true),
        "the keeper holds a custody copy"
    );

    let win_pid = s.kids[w].id();
    sigkill(win_pid);
    let _ = s.kids[w].wait();
    let st = s.await_status("orphaned or relaunched", |t| {
        t.contains("orphaned=1") || t.contains("state=launching")
    });
    assert_eq!(leader_watch.exit_status(), None, "the leader lives:\n{st}");
    assert!(aterm_uds::process::pid_alive(leader));
    let relaunched = within(HANG, Duration::ZERO, || {
        (relaunch_lines(&s) >= 1).then_some(())
    });
    assert!(relaunched.is_some(), "the keeper never relaunched");

    let next = s.spawn(T_CRASH, "next-window", &[]);
    let got = s.line(next, "offers=");
    assert_eq!(
        got,
        format!("offers=1 rdevs={rdev} kept=0"),
        "exactly one OFFER, for that rdev, a crash's"
    );
    let st = s.await_status("offered", |t| t.contains("offered=1"));
    assert!(
        st.contains("state=relaunched"),
        "the HELLO answered the relaunch:\n{st}"
    );
    assert!(st.contains("brake streak=1/3"), "{st}");
    assert_eq!(
        aterm_uds::holders::process_holds_rdev(keeper_pid, rdev),
        Some(true),
        "the keeper still holds its copy after offering"
    );
    assert_eq!(relaunch_lines(&s), 1, "one relaunch");
    assert_eq!(leader_watch.exit_status(), None, "the leader lives");

    s.say(next, "go");
    let _ = s.kids[next].wait();
    let status = await_exit(&leader_watch);
    assert_eq!(
        status, 1,
        "the leader was hung up (SIGHUP), wait status {status:#x}"
    );
    let st = s.await_status("closed", |t| t.contains("masters=0"));
    assert_eq!(relaunch_lines(&s), 1, "a quit relaunches nothing:\n{st}");
    assert_eq!(
        aterm_uds::holders::process_holds_rdev(keeper_pid, rdev),
        Some(false)
    );
}

const T_KEPT: &str = "a_quit_that_keeps_a_shell_offers_it_to_the_next_window_as_kept";

/// A quit whose BYE names its master (the Mac Setup tab's installer,
/// `KeeperCore::bye_keeping`) leaves the shell running with no relaunch, and the next window is
/// offered it as one the quit kept, not a crash's (`Offered::kept`), so its launch tells of no
/// crash; that window's own quit then hangs it up.
///
/// NEGATIVE CONTROL: [`a_killed_window_s_shell_lives_and_is_offered_once`], whose crash's orphan
/// is offered as not kept.
#[test]
fn a_quit_that_keeps_a_shell_offers_it_to_the_next_window_as_kept() {
    dispatch_role();
    let mut s = Scratch::new("kept");
    s.keeper(T_KEPT, false);
    let w = s.spawn(T_KEPT, "window", &[]);
    let reg = s.line(w, "leader=");
    let (leader, rdev) = (field(&reg, "leader") as u32, field(&reg, "rdev"));
    s.pids.push(leader);
    let leader_watch = ExitWatch::watch(leader).expect("watch the leader");
    s.await_status("registered", |t| t.contains("claimed=1"));
    s.say(w, "bye-keep");
    let _ = s.kids[w].wait();
    let st = s.await_status("kept", |t| {
        t.contains("orphaned=1") && t.contains(" kept=1")
    });
    assert_eq!(leader_watch.exit_status(), None, "the leader lives:\n{st}");
    assert_eq!(relaunch_lines(&s), 0, "a quit relaunches nothing:\n{st}");

    let next = s.spawn(T_KEPT, "next-window", &[]);
    assert_eq!(
        s.line(next, "offers="),
        format!("offers=1 rdevs={rdev} kept=1"),
        "offered as the quit's, not a crash's"
    );
    s.say(next, "go");
    let _ = s.kids[next].wait();
    assert_eq!(await_exit(&leader_watch), 1, "its own quit hung it up");
    let st = s.await_status("closed", |t| t.contains("masters=0"));
    assert_eq!(relaunch_lines(&s), 0, "no relaunch throughout:\n{st}");
}

const T_CONTROL: &str = "without_the_custody_copy_a_killed_window_hangs_its_shell_up";

#[test]
fn without_the_custody_copy_a_killed_window_hangs_its_shell_up() {
    dispatch_role();
    let mut s = Scratch::new("control");
    s.keeper(T_CONTROL, true);
    let w = s.spawn(T_CONTROL, "window", &[]);
    let reg = s.line(w, "leader=");
    let leader = field(&reg, "leader") as u32;
    s.pids.push(leader);
    let leader_watch = ExitWatch::watch(leader).expect("watch the leader");
    s.await_status("registered", |t| t.contains("claimed=1"));
    assert!(s.status().contains("custody_copies=0"));
    sigkill(s.kids[w].id());
    let _ = s.kids[w].wait();
    let status = await_exit(&leader_watch);
    assert_eq!(
        status, 1,
        "SIGHUP without a custody copy, wait status {status:#x}"
    );
}

const T_HOLDER: &str = "a_master_another_process_holds_is_never_offered";

#[test]
fn a_master_another_process_holds_is_never_offered() {
    dispatch_role();
    let mut s = Scratch::new("holder");
    s.keeper(T_HOLDER, false);
    let w = s.spawn(T_HOLDER, "window-holder", &[]);
    let reg = s.line(w, "leader=");
    let (leader, rdev, holder) = (
        field(&reg, "leader") as u32,
        field(&reg, "rdev"),
        field(&reg, "holder") as u32,
    );
    s.pids.push(leader);
    s.pids.push(holder);
    assert_eq!(
        aterm_uds::holders::process_holds_rdev(holder, rdev),
        Some(true)
    );
    let leader_watch = ExitWatch::watch(leader).expect("watch the leader");
    s.await_status("registered", |t| t.contains("claimed=1"));
    sigkill(s.kids[w].id());
    let _ = s.kids[w].wait();
    s.await_status("held", |t| t.contains("held=1"));
    let early = s.spawn(T_HOLDER, "next-window", &[]);
    assert_eq!(s.line(early, "offers="), "offers=0 rdevs= kept=0");
    s.say(early, "go");
    let _ = s.kids[early].wait();
    assert_eq!(relaunch_lines(&s), 0, "no relaunch over a live holder");
    let holder_watch = ExitWatch::watch(holder).expect("watch the holder");
    sigkill(holder);
    await_exit(&holder_watch);
    s.await_status("orphaned", |t| {
        t.contains("orphaned=1") || t.contains("state=launching")
    });
    let next = s.spawn(T_HOLDER, "next-window", &[]);
    assert_eq!(
        s.line(next, "offers="),
        format!("offers=1 rdevs={rdev} kept=0")
    );
    assert_eq!(
        leader_watch.exit_status(),
        None,
        "the leader lived throughout"
    );
    s.say(next, "go");
    let _ = s.kids[next].wait();
    assert_eq!(await_exit(&leader_watch), 1, "and the quit hung it up");
}

const T_UNPLACED: &str = "an_offer_the_window_could_not_place_is_released_through_its_link";

#[test]
fn an_offer_the_window_could_not_place_is_released_through_its_link() {
    dispatch_role();
    let mut s = Scratch::new("unplaced");
    s.keeper(T_UNPLACED, false);
    let w = s.spawn(T_UNPLACED, "window", &[]);
    let reg = s.line(w, "leader=");
    let leader = field(&reg, "leader") as u32;
    s.pids.push(leader);
    let leader_watch = ExitWatch::watch(leader).expect("watch the leader");
    s.await_status("registered", |t| t.contains("claimed=1"));
    sigkill(s.kids[w].id());
    let _ = s.kids[w].wait();
    s.await_status("orphaned", |t| t.contains("orphaned=1"));
    let r = s.spawn(T_UNPLACED, "unplaced-window", &[]);
    assert_eq!(s.line(r, "released="), "released=1");
    let ended = s.kids[r].wait().expect("its status");
    assert!(ended.success(), "it quit: {ended}");
    let st = s.await_status("its end read", |t| t.contains("windows=0"));
    assert!(
        st.contains("masters=0") && st.contains("custody_copies=0"),
        "the keeper closed its copy:\n{st}"
    );
    assert_eq!(
        await_exit(&leader_watch),
        1,
        "the shell was hung up (SIGHUP)"
    );
}

const T_QUIET: &str = "a_successor_that_never_registered_hangs_its_shells_up_when_it_quits";

#[test]
fn a_successor_that_never_registered_hangs_its_shells_up_when_it_quits() {
    dispatch_role();
    let mut s = Scratch::new("quiet");
    s.keeper(T_QUIET, false);
    let o = s.spawn(T_QUIET, "outgoing-window", &[]);
    let reg = s.line(o, "leader=");
    let (leader, successor) = (
        field(&reg, "leader") as u32,
        field(&reg, "successor") as u32,
    );
    s.pids.push(leader);
    s.pids.push(successor);
    let leader_watch = ExitWatch::watch(leader).expect("watch the leader");
    let successor_watch = ExitWatch::watch(successor).expect("watch the successor");
    s.say(o, "go");
    let ended = s.kids[o].wait().expect("its status");
    assert!(ended.success(), "the outgoing window exited 0: {ended}");
    s.await_status("held for the successor", |t| t.contains("held=1"));
    s.tell(&format!("kid-{o}-succ"), "go");
    assert_eq!(await_exit(&successor_watch), 0, "the successor quit");
    let st = s.await_status("the hold judged", |t| t.contains("held=0"));
    assert!(
        st.contains("masters=0") && st.contains("custody_copies=0"),
        "the keeper closed its copy:\n{st}"
    );
    assert_eq!(
        await_exit(&leader_watch),
        1,
        "the shell was hung up (SIGHUP)"
    );
}

const T_LOST_BYE: &str = "a_quit_whose_bye_was_lost_hangs_its_shell_up";

#[test]
fn a_quit_whose_bye_was_lost_hangs_its_shell_up() {
    dispatch_role();
    let mut s = Scratch::new("lostbye");
    s.keeper(T_LOST_BYE, false);
    let w = s.spawn(T_LOST_BYE, "marked-window", &[]);
    let reg = s.line(w, "leader=");
    let leader = field(&reg, "leader") as u32;
    s.pids.push(leader);
    let leader_watch = ExitWatch::watch(leader).expect("watch the leader");
    s.await_status("registered, marker kept", |t| {
        t.contains("claimed=1") && t.contains("window_markers=1")
    });
    s.say(w, "go");
    let _ = s.kids[w].wait();
    assert_eq!(
        await_exit(&leader_watch),
        1,
        "the quit hung the leader up (SIGHUP)"
    );
    let st = s.await_status("closed", |t| t.contains("masters=0"));
    assert!(st.contains("ends quit=0 clean_exit=1"), "{st}");
    assert_eq!(relaunch_lines(&s), 0, "a quit relaunches nothing:\n{st}");
}

const T_LOST_BYE_CONTROL: &str = "without_its_marker_a_lost_bye_is_kept_as_an_orphan";

#[test]
fn without_its_marker_a_lost_bye_is_kept_as_an_orphan() {
    dispatch_role();
    let mut s = Scratch::new("lostbye-control");
    s.keeper(T_LOST_BYE_CONTROL, false);
    let w = s.spawn(T_LOST_BYE_CONTROL, "unmarked-window", &[]);
    let reg = s.line(w, "leader=");
    let leader = field(&reg, "leader") as u32;
    s.pids.push(leader);
    let leader_watch = ExitWatch::watch(leader).expect("watch the leader");
    s.await_status("registered, no marker", |t| {
        t.contains("claimed=1") && t.contains("window_markers=0")
    });
    s.say(w, "go");
    let _ = s.kids[w].wait();
    let st = s.await_status("orphaned", |t| t.contains("crash=1"));
    assert_eq!(leader_watch.exit_status(), None, "the leader lives:\n{st}");
}

const T_MARKED_KILL: &str = "a_marked_window_killed_is_an_orphan_and_its_dead_shell_is_pruned";

#[test]
fn a_marked_window_killed_is_an_orphan_and_its_dead_shell_is_pruned() {
    dispatch_role();
    let mut s = Scratch::new("marked-kill");
    s.keeper(T_MARKED_KILL, false);
    let w = s.spawn(T_MARKED_KILL, "marked-window", &[]);
    let reg = s.line(w, "leader=");
    let leader = field(&reg, "leader") as u32;
    s.pids.push(leader);
    let leader_watch = ExitWatch::watch(leader).expect("watch the leader");
    s.await_status("registered, marker kept", |t| {
        t.contains("claimed=1") && t.contains("window_markers=1")
    });
    sigkill(s.kids[w].id());
    let _ = s.kids[w].wait();
    let st = s.await_status("orphaned", |t| t.contains("crash=1"));
    assert!(st.contains("clean_exit=0"), "{st}");
    assert!(
        st.contains(&format!("shell={leader}")),
        "listed while it lives:\n{st}"
    );
    assert_eq!(leader_watch.exit_status(), None, "the leader lives");
    sigkill(leader);
    assert_eq!(await_exit(&leader_watch), 9);
    let st = s.await_status("pruned", |t| t.contains("pruned=1"));
    assert!(st.contains("masters=0"), "{st}");
    assert!(
        !st.contains(&format!("shell={leader}")),
        "never listed dead:\n{st}"
    );
}

/// Where the keeper is parked while the next window writes its frames and exits.
#[derive(Clone, Copy)]
enum Park {
    /// Between two turns: the next turn's poll finds the frames and the exit together, and the
    /// turn's own read takes the frames before it judges the exit.
    BetweenTurns,
    /// Inside a turn, after its poll returned: the turn's read skips the connection, which was
    /// quiet at the poll, and its sweep of the exit watches finds the exit, so only the drain
    /// behind that exit (`Server::drain_pid`) reads the frames before the death is judged.
    InTurn,
}

struct Overtaken {
    /// The keeper's status once the next window's end is judged.
    status: String,
    /// How far the status's `frames_behind_exit` moved over the next window's end.
    behind: u64,
    /// The crashes judged at the next window's end.
    crashes: u64,
    /// The two leaders' exit watches.
    leaders: Vec<ExitWatch>,
    rdevs: Vec<u64>,
    keeper_pid: u32,
}

fn an_exit_that_overtakes_its_frames(
    s: &mut Scratch,
    test: &str,
    role: &str,
    park: Park,
) -> Overtaken {
    let keeper_role = match park {
        Park::BetweenTurns => "keeper-stepped",
        Park::InTurn => "keeper-parked-in-turn",
    };
    let keeper = s.spawn(test, keeper_role, &[]);
    s.line(keeper, "ready");
    let keeper_pid = s.kids[keeper].id();
    let (mut leaders, mut rdevs, mut windows) = (Vec::new(), Vec::new(), Vec::new());
    for _ in 0..2 {
        let w = s.spawn(test, "window", &[]);
        let reg = s.line(w, "leader=");
        let (leader, rdev) = (field(&reg, "leader") as u32, field(&reg, "rdev"));
        s.pids.push(leader);
        leaders.push(ExitWatch::watch(leader).expect("watch the leader"));
        rdevs.push(rdev);
        windows.push(w);
    }
    s.await_status("registered", |t| t.contains("claimed=2"));
    for w in windows {
        sigkill(s.kids[w].id());
        let _ = s.kids[w].wait();
    }
    s.await_status("orphaned", |t| t.contains("orphaned=2"));
    let next = s.spawn(test, role, &[]);
    let got = s.line(next, "offers=");
    assert!(got.starts_with("offers=2 "), "both masters offered: {got}");
    let st = s.await_status("offered", |t| t.contains("offered=2"));
    for w in &leaders {
        assert_eq!(w.exit_status(), None, "the leaders live:\n{st}");
    }
    let crashes_before = ends(&st, "crash");
    let behind_before = status_count(&st, "frames_behind_exit");

    s.pause(keeper);
    s.say(next, "go");
    let ended = s.kids[next].wait().expect("the next window's status");
    assert!(ended.success(), "the next window exited 0: {ended}");
    s.resume(keeper);
    let status = s.await_status("the window's end judged", |t| {
        t.contains("claimed=0") && t.contains("offered=0") && t.contains("windows=0")
    });
    let behind = status_count(&status, "frames_behind_exit") - behind_before;
    let crashes = ends(&status, "crash") - crashes_before;
    Overtaken {
        status,
        behind,
        crashes,
        leaders,
        rdevs,
        keeper_pid,
    }
}

fn holders(rdevs: &[u64]) -> String {
    rdevs
        .iter()
        .map(|r| {
            let scan = aterm_uds::holders::scan_rdev_holders(*r, &[]);
            format!("{r}: {:?}", scan.map(|s| s.holders))
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn ends(status: &str, key: &str) -> u64 {
    let line = status
        .lines()
        .find(|l| l.starts_with("ends "))
        .unwrap_or_else(|| panic!("an ends line in\n{status}"));
    field(line, key)
}

const T_OVERTAKE: &str = "a_quit_whose_exit_overtakes_its_claims_hangs_both_shells_up";

/// The next window claims both offered masters, says BYE and exits while the keeper is parked
/// inside a turn after its poll ([`Park::InTurn`]), so the turn's sweep of its exit watches
/// finds the exit with all three frames still unread: the drain behind it hands the core both
/// REGISTERs and the BYE before the death is judged, and the quit hangs both shells up. Without
/// that drain the exit is judged first, as a crash with its claims unread.
#[test]
fn a_quit_whose_exit_overtakes_its_claims_hangs_both_shells_up() {
    dispatch_role();
    let mut s = Scratch::new("overtake");
    let o = an_exit_that_overtakes_its_frames(&mut s, T_OVERTAKE, "next-window", Park::InTurn);
    let st = &o.status;
    assert!(
        st.contains("masters=0"),
        "both claims read, then the quit:\n{st}\nholders (the keeper is {}): {}",
        o.keeper_pid,
        holders(&o.rdevs)
    );
    assert!(st.contains("custody_copies=0"), "{st}");
    assert_eq!(ends(st, "quit"), 2, "{st}");
    assert_eq!(o.crashes, 0, "{st}");
    assert_eq!(
        o.behind, 3,
        "both REGISTERs and the BYE drained behind the exit:\n{st}"
    );
    for w in &o.leaders {
        assert_eq!(await_exit(w), 1, "the leader was hung up (SIGHUP)");
    }
    for rdev in &o.rdevs {
        assert_eq!(
            aterm_uds::holders::process_holds_rdev(o.keeper_pid, *rdev),
            Some(false)
        );
    }
}

const T_OVERTAKE_CONTROL: &str = "without_a_bye_an_overtaken_claim_keeps_both_shells";

/// The control of [`a_quit_whose_exit_overtakes_its_claims_hangs_both_shells_up`]: the same
/// overtaken exit with no BYE. Both claims are drained behind it and the death is a crash, so
/// both shells are kept as orphans.
#[test]
fn without_a_bye_an_overtaken_claim_keeps_both_shells() {
    dispatch_role();
    let mut s = Scratch::new("overtake-ctl");
    let o = an_exit_that_overtakes_its_frames(
        &mut s,
        T_OVERTAKE_CONTROL,
        "next-window-no-bye",
        Park::InTurn,
    );
    let st = &o.status;
    assert!(st.contains("orphaned=2"), "{st}");
    assert!(st.contains("custody_copies=2"), "{st}");
    assert_eq!(o.crashes, 2, "both claims read, then judged a crash:\n{st}");
    assert_eq!(o.behind, 2, "both REGISTERs drained behind the exit:\n{st}");
    for w in &o.leaders {
        assert_eq!(w.exit_status(), None, "the leader lives:\n{st}");
    }
}

fn status_count(status: &str, key: &str) -> u64 {
    status
        .lines()
        .find_map(|l| l.strip_prefix(&format!("{key}=")))
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(|| panic!("{key} in\n{status}"))
}

const T_REPLY: &str = "a_reply_the_dead_window_cannot_read_loses_none_of_its_frames";

#[test]
fn a_reply_the_dead_window_cannot_read_loses_none_of_its_frames() {
    dispatch_role();
    let mut s = Scratch::new("reply");
    let o =
        an_exit_that_overtakes_its_frames(&mut s, T_REPLY, "next-window-adopt", Park::BetweenTurns);
    let st = &o.status;
    assert!(
        st.contains("masters=0"),
        "every frame behind the unwritable reply read, then the quit:\n{st}"
    );
    assert!(st.contains("custody_copies=0"), "{st}");
    assert_eq!(ends(st, "quit"), 2, "{st}");
    assert_eq!(o.crashes, 0, "{st}");
    assert!(status_count(st, "replies_lost") >= 1, "{st}");
    for w in &o.leaders {
        assert_eq!(await_exit(w), 1, "the leader was hung up (SIGHUP)");
    }
}

const T_REPLY_CONTROL: &str = "without_a_bye_a_reply_lost_to_the_dead_keeps_both_shells";

#[test]
fn without_a_bye_a_reply_lost_to_the_dead_keeps_both_shells() {
    dispatch_role();
    let mut s = Scratch::new("reply-ctl");
    let o = an_exit_that_overtakes_its_frames(
        &mut s,
        T_REPLY_CONTROL,
        "next-window-adopt-no-bye",
        Park::BetweenTurns,
    );
    let st = &o.status;
    assert!(st.contains("orphaned=2"), "{st}");
    assert!(st.contains("custody_copies=2"), "{st}");
    assert_eq!(o.crashes, 2, "both claims read, then judged a crash:\n{st}");
    assert!(status_count(st, "replies_lost") >= 1, "{st}");
    for w in &o.leaders {
        assert_eq!(w.exit_status(), None, "the leader lives:\n{st}");
    }
}

const T_DEAD_PEER: &str = "a_quit_beside_a_dead_unjudged_peer_hangs_its_shell_up";

#[test]
fn a_quit_beside_a_dead_unjudged_peer_hangs_its_shell_up() {
    dispatch_role();
    let mut s = Scratch::new("dead-peer");
    let keeper = s.spawn(T_DEAD_PEER, "keeper-stepped", &[]);
    s.line(keeper, "ready");
    let mut windows = Vec::new();
    for _ in 0..2 {
        let w = s.spawn(T_DEAD_PEER, "window", &[]);
        let reg = s.line(w, "leader=");
        let leader = field(&reg, "leader") as u32;
        s.pids.push(leader);
        let watch = ExitWatch::watch(leader).expect("watch the leader");
        windows.push((s.kids[w].id(), w, watch));
    }
    s.await_status("registered", |t| t.contains("claimed=2"));
    windows.sort_by_key(|(pid, _, _)| *pid);
    let (killed, quitter) = (windows.pop().expect("two"), windows.pop().expect("two"));
    let (killed_pid, killed_kid, killed_leader) = killed;
    let (_, quitter_kid, quitter_leader) = quitter;

    s.pause(keeper);
    s.say(killed_kid, "poke");
    s.await_note(killed_kid, "poked");
    let watch = ExitWatch::watch(killed_pid).expect("watch the killed window");
    sigkill(killed_pid);
    await_exit(&watch);
    assert!(
        aterm_uds::process::pid_alive(killed_pid),
        "unreaped: still in the process list"
    );
    s.say(quitter_kid, "bye");
    let ended = s.kids[quitter_kid].wait().expect("the quitter's status");
    assert!(ended.success(), "the quitter exited 0: {ended}");
    s.resume(keeper);

    let st = s.await_status("both ends judged", |t| {
        t.contains("windows=0") && t.contains("claimed=0")
    });
    assert_eq!(ends(&st, "handoff"), 0, "no successor holds it:\n{st}");
    assert_eq!(ends(&st, "quit"), 1, "{st}");
    assert_eq!(
        await_exit(&quitter_leader),
        1,
        "the quit hung its shell up (SIGHUP)"
    );
    assert_eq!(ends(&st, "crash"), 1, "{st}");
    assert!(
        st.contains("masters=1") && st.contains("orphaned=1"),
        "{st}"
    );
    assert_eq!(
        killed_leader.exit_status(),
        None,
        "the killed window's shell lives"
    );
}

/// A restarting keeper (`restarting_keeper_role`) run from a copy of this test binary
/// ([`copied_keeper`]), once it reported it serves.
fn restarting_keeper(s: &mut Scratch, test: &str) -> (usize, KeptUntilRan, PathBuf) {
    let (keeper, bin, copy) = copied_keeper(s, test, "keeper-restarting", &[]);
    s.line(keeper, "ready");
    (keeper, bin, copy)
}

/// A keeper `role` run from a copy of this test binary ([`keeper_copy`]), once its `ready` line
/// on stdout showed it runs from the copy: from then on the copy may be renamed over. The
/// directory, and the copy's path.
fn copied_keeper(
    s: &mut Scratch,
    test: &str,
    role: &str,
    extra: &[(&str, &str)],
) -> (usize, KeptUntilRan, PathBuf) {
    let (mut bin, copy) = keeper_copy();
    let keeper = run_copied(s, &mut bin, &copy, test, role, extra);
    (keeper, bin, copy)
}

/// Spawn `role` from `copy` (in `bin`) and wait for its `ready` line on stdout, which proves its
/// exec ran from the copy; `bin` may go from then on.
fn run_copied(
    s: &mut Scratch,
    bin: &mut KeptUntilRan,
    copy: &Path,
    test: &str,
    role: &str,
    extra: &[(&str, &str)],
) -> usize {
    let keeper = s.spawn_from(copy, test, role, extra, true);
    aterm_uds::deadline::prints(&mut s.kids[keeper], "ready", FIRST_EXEC_HANG);
    bin.ran();
    keeper
}

/// A copy of this test binary in a directory of its own laid out as a bundle's `Contents`
/// (`MacOS/<copy>` beside an `Info.plist` naming [`KEEPER_VERSION`], so a file renamed over the
/// copy reads as a build as new as the keeper), under the binary's own name (a test binary's
/// isolation, `aterm_uds::test_binary`). The directory, and the copy's path.
fn keeper_copy() -> (KeptUntilRan, PathBuf) {
    let own = std::env::current_exe().expect("test binary");
    let bin = KeptUntilRan::new(
        aterm_tempfile::Builder::new()
            .prefix("akr-")
            .suffix(".noindex")
            .tempdir_in("/tmp")
            .expect("scratch")
            .keep(),
    );
    let macos = bin.path().join("MacOS");
    std::fs::create_dir(&macos).expect("the copy's directory");
    std::fs::write(
        bin.path().join("Info.plist"),
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\">\n<dict>\n\t\
             <key>CFBundleShortVersionString</key>\n\t<string>{KEEPER_VERSION}</string>\n\
             </dict>\n</plist>\n"
        ),
    )
    .expect("the bundle's Info.plist");
    let copy = macos.join(own.file_name().expect("its name"));
    std::fs::copy(&own, &copy).expect("copy the test binary");
    (bin, copy)
}

/// What an update does to the keeper's executable: another file renamed over its path.
fn replace(exe: &Path) {
    let next = exe.with_extension("next");
    std::fs::write(&next, b"the build an update put at the keeper's path").expect("write");
    std::fs::rename(&next, exe).expect("rename over the keeper's executable");
}

/// The `window` role `window` asks its keeper to restart (`poke` sends RESTART_WHEN_SAFE) and
/// stays connected, holding its master, until it is killed.
fn ask_restart(s: &mut Scratch, window: usize) {
    s.say(window, "poke");
    s.await_note(window, "poked");
}

const T_RESTART: &str = "a_keeper_asked_to_restart_beside_live_windows_alone_exits_once_replaced";

/// A newer window's RESTART_WHEN_SAFE (`KeeperCore::restart_when_safe`): with every master held
/// by a connected window, which tells a restarted keeper again, the keeper exits 0 at once, for
/// launchd to start the file now at its path, and the window's shell lives on. Before its
/// executable is replaced, nobody asked; once asked, it went because another file stood there.
///
/// NEGATIVE CONTROL: [`a_keeper_asked_to_restart_beside_an_orphan_stays_until_its_shell_ends`],
/// where an orphan only the keeper holds keeps it running past the same ask.
#[test]
fn a_keeper_asked_to_restart_beside_live_windows_alone_exits_once_replaced() {
    dispatch_role();
    let mut s = Scratch::new("restart");
    let (keeper, _bin, copy) = restarting_keeper(&mut s, T_RESTART);
    let keeper_pid = s.kids[keeper].id();
    let w = s.spawn(T_RESTART, "window", &[]);
    let reg = s.line(w, "leader=");
    let leader = field(&reg, "leader") as u32;
    s.pids.push(leader);
    let leader_watch = ExitWatch::watch(leader).expect("watch the leader");
    s.await_status("registered", |t| {
        t.contains("claimed=1") && t.contains("image=same")
    });
    replace(&copy);
    let st = s.await_status("replaced", |t| t.contains("image=replaced"));
    assert!(st.contains("restart=unwanted"), "nobody asked yet:\n{st}");
    let window_pid = s.kids[w].id();
    ask_restart(&mut s, w);
    let ended = until_while_working(
        keeper_pid,
        "the keeper exits for its new executable",
        || s.kids[keeper].try_wait().ok().flatten(),
    );
    assert_eq!(ended.code(), Some(0), "a restart exits 0: {ended}");
    let log = std::fs::read_to_string(s.log_of(keeper)).expect("its log");
    for want in [
        format!("keeper: restart: wanted by window pid {window_pid}"),
        format!("keeper: restart: exiting for {} replaced", copy.display()),
    ] {
        assert!(log.contains(&want), "{want:?} in\n{log}");
    }
    assert_eq!(leader_watch.exit_status(), None, "the window's shell lives");
    assert!(aterm_uds::process::pid_alive(leader));
}

const T_RESTART_HELD: &str =
    "a_keeper_asked_to_restart_beside_an_orphan_stays_until_its_shell_ends";

/// The negative control of [`a_keeper_asked_to_restart_beside_live_windows_alone_exits_once_replaced`]:
/// a killed window's orphan only the keeper holds keeps it running past the ask, with its
/// executable replaced; once the orphan's shell ends and is pruned, it exits 0, the asking
/// window's shell living on.
#[test]
fn a_keeper_asked_to_restart_beside_an_orphan_stays_until_its_shell_ends() {
    dispatch_role();
    let mut s = Scratch::new("restart-held");
    let (keeper, _bin, copy) = restarting_keeper(&mut s, T_RESTART_HELD);
    let keeper_pid = s.kids[keeper].id();
    let mut windows = Vec::new();
    for _ in 0..2 {
        let w = s.spawn(T_RESTART_HELD, "window", &[]);
        let reg = s.line(w, "leader=");
        let leader = field(&reg, "leader") as u32;
        s.pids.push(leader);
        windows.push((
            w,
            leader,
            ExitWatch::watch(leader).expect("watch the leader"),
        ));
    }
    s.await_status("registered", |t| t.contains("claimed=2"));
    let (killed, orphan, orphan_watch) = windows.pop().expect("two");
    let (asker, _, asker_watch) = windows.pop().expect("two");
    sigkill(s.kids[killed].id());
    let _ = s.kids[killed].wait();
    s.await_status("orphaned", |t| t.contains("orphaned=1"));
    replace(&copy);
    s.await_status("replaced", |t| t.contains("image=replaced"));
    ask_restart(&mut s, asker);
    let st = s.await_status("held by the orphan", |t| {
        t.contains("restart=wanted askers=1 held_by=orphaned")
    });
    assert!(
        s.kids[keeper].try_wait().expect("its status").is_none(),
        "the keeper stays while the orphan lives:\n{st}"
    );
    assert_eq!(orphan_watch.exit_status(), None, "the orphan's shell lives");

    sigkill(orphan);
    assert_eq!(await_exit(&orphan_watch), 9);
    let ended = until_while_working(
        keeper_pid,
        "the keeper exits once the orphan is pruned",
        || s.kids[keeper].try_wait().ok().flatten(),
    );
    assert_eq!(ended.code(), Some(0), "a restart exits 0: {ended}");
    let log = std::fs::read_to_string(s.log_of(keeper)).expect("its log");
    assert!(
        log.contains("keeper: restart: held by 1 orphaned master"),
        "what held it is said:\n{log}"
    );
    assert_eq!(
        asker_watch.exit_status(),
        None,
        "the asking window's shell lives"
    );
}

const T_SUPERVISED: &str = "only_a_keeper_launchd_runs_as_its_job_honours_a_restart";

/// The test label a `keeper-cli` keeper's `--job` names.
const JOB: &str = "com.aterm.aterm.keeper.test.restart";

/// `aterm keeper serve` honours RESTART_WHEN_SAFE only as its launchd job (launchd sets
/// `XPC_SERVICE_NAME` to the label of each job it runs), since only launchd starts it again: the
/// label its `--job` names, else its place's (`job::place_for` of its executable, the label
/// `start` submits with no flag). Run so, a window's ask, with its executable replaced by a
/// build as new, ends it, 0; under another service name (a keeper run by hand), or with `--job`
/// naming another job than launchd's, its WELCOME states no CAP_RESTARTS and the same ask leaves
/// it running, saying why. `None` below is the keeper's place's label.
///
/// NEGATIVE CONTROLS (run by hand, each restored): with `serve_as` calling `restart_for`
/// whatever `supervised` says, the second and fourth keepers state CAP_RESTARTS and exit; with
/// `supervised` ignoring the place (the flag required again), the first keeper, flagless under
/// its place's label, states none.
#[test]
fn only_a_keeper_launchd_runs_as_its_job_honours_a_restart() {
    dispatch_role();
    for (job, xpc, honours) in [
        (None, None, true),
        (None, Some("com.aterm.aterm.keeper.test.other"), false),
        (Some(JOB), Some(JOB), true),
        (Some(JOB), None, false),
    ] {
        let mut s = Scratch::new("supervised");
        let home = s.dir.join("home");
        std::fs::create_dir(&home).expect("a scratch HOME");
        let home = home.to_str().expect("a UTF-8 path").to_owned();
        let (mut bin, copy) = keeper_copy();
        let place = aterm_keeper::job::place_for(&copy.canonicalize().expect("its path")).label;
        let xpc = xpc.unwrap_or(&place);
        let mut extra = vec![("XPC_SERVICE_NAME", xpc), ("HOME", home.as_str())];
        extra.extend(job.map(|job| (CLI_JOB, job)));
        let keeper = run_copied(&mut s, &mut bin, &copy, T_SUPERVISED, "keeper-cli", &extra);
        let keeper_pid = s.kids[keeper].id();
        let (st, restarts) = within(HANG, Duration::from_millis(5), || {
            let c = KeeperClient::connect(&s.sock(), &Identity::same_uid(), HANG).ok()?;
            let st = c.status("test").ok()?;
            Some((st, c.keeper_restarts()))
        })
        .expect("the keeper never answered");
        let case = format!("job {job:?} as {xpc}");
        let image = if honours { "image=same" } else { "image=none" };
        assert!(st.contains(image), "{case}: {image} in\n{st}");
        assert_eq!(restarts, honours, "{case}: its WELCOME's CAP_RESTARTS");
        let w = s.spawn(T_SUPERVISED, "window", &[]);
        let reg = s.line(w, "leader=");
        s.pids.push(field(&reg, "leader") as u32);
        replace(&copy);
        ask_restart(&mut s, w);
        if honours {
            let ended = until_while_working(keeper_pid, "the keeper exits for its job", || {
                s.kids[keeper].try_wait().ok().flatten()
            });
            assert_eq!(ended.code(), Some(0), "{case}: a restart exits 0: {ended}");
            continue;
        }
        let st = s.await_status("the ask held", |t| {
            t.contains("restart=wanted askers=1 held_by=image-unknown")
        });
        assert!(
            s.kids[keeper].try_wait().expect("its status").is_none(),
            "{case}: nothing would start it again, so it stays:\n{st}"
        );
        let log = std::fs::read_to_string(s.log_of(keeper)).expect("its log");
        assert!(
            log.contains(
                "so nothing would start this keeper again; a window's RESTART_WHEN_SAFE \
                 is never honoured"
            ),
            "{case}: why is said:\n{log}"
        );
    }
}

#[test]
fn a_status_client_reads_the_keeper() {
    dispatch_role();
    let mut s = Scratch::new("status");
    s.keeper("a_status_client_reads_the_keeper", false);
    let st = s.status();
    for want in [
        "keeper=running",
        "identity=same-uid",
        "masters=0",
        "claimed=0 held=0 orphaned=0 offered=0",
        "relaunch=available state=idle relaunches=0",
        "brake streak=0/3 held=0",
    ] {
        assert!(st.contains(want), "{want:?} in\n{st}");
    }
}

const JOURNAL_ROWS: u16 = 12;
const JOURNAL_COLS: u16 = 48;
const JOURNAL_CONFIG: u64 = 0x0a7e_c0de;
const KEYFRAMED: usize = 20;

fn journal_output() -> Vec<Vec<u8>> {
    let mut out: Vec<Vec<u8>> = (0..40)
        .map(|i| format!("\x1b[3{}mline {i:03} of the dead window\x1b[0m\r\n", i % 8).into_bytes())
        .collect();
    out.push("$ \u{2713} last words \u{4e16}\u{754c}".as_bytes().to_vec());
    out
}

fn journal_screen(slices: &[Vec<u8>]) -> String {
    let mut term = aterm_core::terminal::Terminal::new(JOURNAL_ROWS, JOURNAL_COLS);
    for slice in slices {
        term.process(slice);
    }
    term.visible_content()
}

/// What a journaling window registers beside its master.
#[derive(Clone, Copy)]
enum Journaled {
    /// A region it writes the session into.
    Region,
    /// Nothing: its claim stays unsettled.
    Nothing,
    /// A region whose custody descriptor is open for writing, which the keeper refuses (§3.2
    /// N8), and then a proper one.
    Writable,
}

fn journal_keyframe(
    writer: &mut aterm_restore::JournalWriter<aterm_uds::shm::Region>,
    term: &aterm_core::terminal::Terminal,
) {
    let checkpoint = term.checkpoint_journal(16).expect("a keyframe at ground");
    writer
        .write_keyframe(&aterm_restore::KeyframeInput {
            checkpoint: &checkpoint,
            fence: term.history_fence(),
            resize_lifetime: term.resize_lifetime(),
            resize_ordinal: term.resize_ordinal(),
            bridge_lines: 16,
            config_fp: JOURNAL_CONFIG,
            integration_on: term.shell_integration_posture()
                == aterm_core::terminal::ShellIntegrationPosture::On,
        })
        .expect("the keyframe");
}

fn journal_window_role(journaled: Journaled) -> ! {
    use aterm_keeper::wire::RefuseCode;
    let socket = PathBuf::from(std::env::var(SOCK).expect("socket"));
    let (master, leader) = pty_with_leader();
    let rdev = rdev_of(&master);
    let birth = born(leader.id());
    let client = KeeperClient::connect(&socket, &Identity::same_uid(), HANG).expect("connect");
    let offers = client.hello_app_marked("test", None, true).expect("hello");
    assert!(offers.is_empty(), "a fresh window is offered nothing");
    assert!(client.keeper_journals(), "the keeper says it takes regions");
    let dup = master.try_clone().expect("dup for the keeper");
    client
        .register(
            &dup,
            MasterHeader {
                rdev,
                shell_pid: leader.id(),
                shell_birth: birth,
                local_id: 1,
            },
            b"sid=s-test".to_vec(),
        )
        .expect("register");
    drop(dup);
    let len = aterm_restore::REGION_LEN;
    let mut line = format!("leader={} rdev={rdev}", leader.id());
    let mut kept = Vec::new();
    let mut writer = None;
    match journaled {
        Journaled::Region => {
            #[expect(
                clippy::disallowed_methods,
                reason = "a test playing the window makes the region the keeper is sent"
            )]
            // SAFETY: single-writer `JournalWriter` in this test process; keeper holds read-only custody.
            let (region, custody) =
                unsafe { aterm_uds::shm::Region::create(len) }.expect("a region");
            let mut journal = aterm_restore::JournalWriter::create(
                region,
                aterm_restore::Geometry::v1(),
                &aterm_restore::WriterIdentity {
                    pid: std::process::id(),
                    birth_us: 0,
                    local_id: 1,
                    build: 0,
                    sid: "s-test",
                },
            )
            .expect("the journal");
            let mut term = aterm_core::terminal::Terminal::new(JOURNAL_ROWS, JOURNAL_COLS);
            journal_keyframe(&mut journal, &term);
            let mut gathered = 0u64;
            for (i, slice) in journal_output().iter().enumerate() {
                if i == KEYFRAMED {
                    journal_keyframe(&mut journal, &term);
                }
                gathered += slice.len() as u64;
                journal.set_gathered(gathered).expect("gathered");
                term.process(slice);
                journal.append_slice(slice).expect("appended");
            }
            client
                .companion(rdev, &custody, len as u64)
                .expect("the companion");
            kept.push(custody);
            writer = Some(journal);
        }
        Journaled::Nothing => {}
        Journaled::Writable => {
            #[expect(
                clippy::disallowed_methods,
                reason = "a test playing the window makes the region the keeper is sent"
            )]
            // SAFETY: this fixture keeper checks metadata and refuses the writable
            // descriptor before any mapping; no writer uses it or its aliases. The local
            // mapping remains inert until both local owners are dropped below.
            let (region, writable) =
                unsafe { aterm_uds::shm::testing::create_with_writable_custody(len) }
                    .expect("a twin");
            client
                .companion(rdev, &writable, len as u64)
                .expect("the writable companion");
            let answer = client.recv().expect("the keeper answers").map(|i| i.frame);
            match answer {
                Some(Frame::Refused { code, rdev: of }) if of == rdev => {
                    line.push_str(&format!(" refused={code:?}"));
                    assert_eq!(code, RefuseCode::NotAJournal);
                }
                other => panic!("expected the refusal, got {other:?}"),
            }
            #[expect(
                clippy::disallowed_methods,
                reason = "a test playing the window makes the region the keeper is sent"
            )]
            // SAFETY: the fixture keeper only holds/queries this custody; the local
            // mapping is inert and dropped below. No participant writes or resizes aliases.
            let (proper, custody) =
                unsafe { aterm_uds::shm::Region::create(len) }.expect("a region");
            client
                .companion(rdev, &custody, len as u64)
                .expect("the companion");
            drop((region, writable, proper));
            kept.push(custody);
        }
    }
    report(&line);
    std::mem::forget(leader);
    match await_go().as_str() {
        "die" => aterm_uds::process::kill_self_now(),
        _ => client
            .send(&Frame::Bye { keep: Vec::new() }, None)
            .expect("bye"),
    }
    drop((writer, kept, master));
    std::process::exit(0)
}

fn journal_next_window_role() -> ! {
    use std::os::fd::AsFd as _;
    let socket = PathBuf::from(std::env::var(SOCK).expect("socket"));
    let talk = PathBuf::from(std::env::var(TALK).expect("talk"));
    let client = KeeperClient::connect(&socket, &Identity::same_uid(), HANG).expect("connect");
    let offers = client.hello_app_marked("test", None, true).expect("hello");
    let mut line = format!("offers={}", offers.len());
    for o in &offers {
        assert_eq!(rdev_of(&o.master), o.header.rdev, "the offered master");
        line.push_str(&format!(
            " companions={} attempts={}",
            u8::from(o.companion.is_some()),
            o.attempts
        ));
        let Some(c) = &o.companion else {
            line.push_str(" restore=meta");
            continue;
        };
        let len = usize::try_from(c.len).expect("a region length");
        #[expect(
            clippy::disallowed_methods,
            reason = "a test playing the restoring window maps the region the keeper offered"
        )]
        // SAFETY: writer process has already been killed and reaped before mapping read-only.
        let region = unsafe { aterm_uds::shm::Region::map_readonly(c.region.as_fd(), len) }
            .expect("mapped read-only");
        let policy = aterm_restore::RecoverPolicy {
            tail: o.attempts == 0,
            config_fp: JOURNAL_CONFIG,
            scratch_nonce: [0xA5; 32],
        };
        match aterm_restore::recover(&region, policy, aterm_core::terminal::Terminal::new) {
            aterm_restore::Recovery::Restored(r) => {
                let screen = aterm_core::terminal::Terminal::from_checkpoint(&r.checkpoint)
                    .visible_content();
                std::fs::write(talk.with_extension("screen"), screen).expect("the screen");
                line.push_str(if r.exact {
                    " restore=exact"
                } else {
                    " restore=screen"
                });
            }
            aterm_restore::Recovery::Meta(why) => line.push_str(&format!(" restore=meta({why:?})")),
        }
    }
    report(&line);
    let then = await_go();
    for o in &offers {
        client
            .register(&o.master, o.header, o.tag.clone())
            .expect("claim");
    }
    if then == "die-claimed" {
        aterm_uds::process::kill_self_now();
    }
    for o in &offers {
        client
            .companion_release(o.header.rdev)
            .expect("no region of its own");
    }
    client
        .send(&Frame::Bye { keep: Vec::new() }, None)
        .expect("bye");
    std::process::exit(0)
}

fn killed(status: std::process::ExitStatus) -> bool {
    use std::os::unix::process::ExitStatusExt as _;
    status.signal() == Some(9)
}

fn journal_window_dies(s: &mut Scratch, w: usize) {
    s.say(w, "die");
    let ended = s.kids[w].wait().expect("its status");
    assert!(killed(ended), "the window died by SIGKILL: {ended}");
    s.await_status("orphaned", |t| {
        t.contains("orphaned=1") || t.contains("state=launching")
    });
}

fn journal_window(s: &mut Scratch, test: &str, role: &str, journals: &str) -> (usize, u32, u64) {
    let w = s.spawn(test, role, &[]);
    let reg = s.line(w, "leader=");
    let (leader, rdev) = (field(&reg, "leader") as u32, field(&reg, "rdev"));
    s.pids.push(leader);
    s.await_status("registered", |t| {
        t.contains("claimed=1") && t.contains(journals)
    });
    (w, leader, rdev)
}

const T_JOURNAL: &str = "a_killed_window_s_region_restores_its_exact_screen";

#[test]
fn a_killed_window_s_region_restores_its_exact_screen() {
    dispatch_role();
    let mut s = Scratch::new("journal");
    s.keeper(T_JOURNAL, false);
    let (w, leader, _) = journal_window(
        &mut s,
        T_JOURNAL,
        "journal-window",
        "journals=1 virtual_mib=8",
    );
    assert_eq!(keeper_regions(&s), [REGION], "the keeper holds the region");
    let leader_watch = ExitWatch::watch(leader).expect("watch the leader");
    journal_window_dies(&mut s, w);
    assert_eq!(
        keeper_regions(&s),
        [REGION],
        "the dead window's region outlives it"
    );
    let next = s.spawn(T_JOURNAL, "journal-next-window", &[]);
    assert_eq!(
        s.line(next, "offers="),
        "offers=1 companions=1 attempts=0 restore=exact"
    );
    let screen = std::fs::read_to_string(s.dir.join(format!("kid-{next}.screen")))
        .expect("the restored screen");
    assert_eq!(
        screen,
        journal_screen(&journal_output()),
        "the dead window's screen"
    );
    assert_eq!(leader_watch.exit_status(), None, "the leader lives");
    s.say(next, "quit");
    let ended = s.kids[next].wait().expect("its status");
    assert!(ended.success(), "it quit: {ended}");
    assert_eq!(await_exit(&leader_watch), 1, "the quit hung the shell up");
    let st = s.await_status("closed", |t| t.contains("masters=0"));
    assert!(
        st.contains("journals=0 virtual_mib=0"),
        "the region closed:\n{st}"
    );
    assert_eq!(keeper_regions(&s), [], "and its descriptor with it");
}

const T_JOURNAL_BARE: &str = "a_window_that_registered_no_region_is_offered_none";

#[test]
fn a_window_that_registered_no_region_is_offered_none() {
    dispatch_role();
    let mut s = Scratch::new("journal-bare");
    s.keeper(T_JOURNAL_BARE, false);
    let (w, _, _) = journal_window(&mut s, T_JOURNAL_BARE, "journal-window-bare", "journals=0");
    assert_eq!(keeper_regions(&s), [], "nothing was sent");
    journal_window_dies(&mut s, w);
    let next = s.spawn(T_JOURNAL_BARE, "journal-next-window", &[]);
    assert_eq!(
        s.line(next, "offers="),
        "offers=1 companions=0 attempts=1 restore=meta"
    );
    s.say(next, "quit");
    let _ = s.kids[next].wait();
    s.await_status("closed", |t| t.contains("masters=0"));
}

const T_JOURNAL_POISON: &str = "a_restorer_that_dies_before_its_companion_costs_an_attempt";

#[test]
fn a_restorer_that_dies_before_its_companion_costs_an_attempt() {
    dispatch_role();
    let mut s = Scratch::new("journal-poison");
    s.keeper(T_JOURNAL_POISON, false);
    let (w, _, _) = journal_window(&mut s, T_JOURNAL_POISON, "journal-window", "journals=1");
    journal_window_dies(&mut s, w);
    let first = s.spawn(T_JOURNAL_POISON, "journal-next-window", &[]);
    assert_eq!(
        s.line(first, "offers="),
        "offers=1 companions=1 attempts=0 restore=exact"
    );
    s.say(first, "die-claimed");
    let ended = s.kids[first].wait().expect("its status");
    assert!(killed(ended), "the restorer died by SIGKILL: {ended}");
    s.await_status("orphaned again", |t| t.contains("orphaned=1"));
    assert_eq!(
        keeper_regions(&s),
        [REGION],
        "the region is kept for one more try"
    );
    let second = s.spawn(T_JOURNAL_POISON, "journal-next-window", &[]);
    assert_eq!(
        s.line(second, "offers="),
        "offers=1 companions=1 attempts=1 restore=screen"
    );
    let screen = std::fs::read_to_string(s.dir.join(format!("kid-{second}.screen")))
        .expect("the restored screen");
    assert_eq!(
        screen,
        journal_screen(&journal_output()[..KEYFRAMED]),
        "the keyframe alone"
    );
    s.say(second, "quit");
    let _ = s.kids[second].wait();
    s.await_status("closed", |t| t.contains("masters=0"));
    assert_eq!(keeper_regions(&s), [], "closed with the master");
}

const T_JOURNAL_OLD: &str = "a_window_that_does_not_journal_is_offered_the_master_alone";

#[test]
fn a_window_that_does_not_journal_is_offered_the_master_alone() {
    dispatch_role();
    let mut s = Scratch::new("journal-old");
    s.keeper(T_JOURNAL_OLD, false);
    let (w, _, rdev) = journal_window(&mut s, T_JOURNAL_OLD, "journal-window", "journals=1");
    journal_window_dies(&mut s, w);
    let old = s.spawn(T_JOURNAL_OLD, "old-window", &[]);
    assert_eq!(
        s.line(old, "offers="),
        format!("offers=1 trailers=0 strays=0 rdevs={rdev}")
    );
    assert_eq!(
        keeper_regions(&s),
        [REGION],
        "the region is kept for a window that journals"
    );
    s.say(old, "claim");
    let st = s.await_status("the old window's claim", |t| t.contains("claimed=1"));
    assert!(st.contains("journals=0"), "{st}");
    assert_eq!(keeper_regions(&s), [], "its claim closed the region (N5)");
    s.say(old, "quit");
    let ended = s.kids[old].wait().expect("its status");
    assert!(ended.success(), "its quit succeeded: {ended}");
    s.await_status("closed", |t| t.contains("masters=0"));
}

const T_JOURNAL_WRITABLE: &str = "a_writable_custody_descriptor_is_refused";

#[test]
fn a_writable_custody_descriptor_is_refused() {
    dispatch_role();
    let mut s = Scratch::new("journal-writable");
    s.keeper(T_JOURNAL_WRITABLE, false);
    let w = s.spawn(T_JOURNAL_WRITABLE, "journal-window-writable", &[]);
    let reg = s.line(w, "leader=");
    s.pids.push(field(&reg, "leader") as u32);
    assert!(reg.ends_with("refused=NotAJournal"), "{reg}");
    s.await_status("only the read-only region held", |t| {
        t.contains("claimed=1") && t.contains("journals=1 virtual_mib=8")
    });
    assert_eq!(
        keeper_regions(&s),
        [REGION],
        "the writable one was not kept"
    );
    s.say(w, "quit");
    let _ = s.kids[w].wait();
    s.await_status("closed", |t| {
        t.contains("masters=0") && t.contains("journals=0")
    });
    assert_eq!(keeper_regions(&s), [], "closed with the master");
}
