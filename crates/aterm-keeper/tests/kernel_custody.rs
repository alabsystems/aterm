// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE KEEPER ON A REAL KERNEL (`docs/DESIGN-pty-keeper-2026-09-26.md` §6.4):
//! a keeper process, "window" processes that own real PTY masters and pass
//! duplicates by `SCM_RIGHTS` over the keeper's real socket, and a session
//! leader on each slave that the kernel hangs up when the master's last copy
//! closes. No WindowServer: every process here is this test binary re-run in a
//! role, or `/bin/sleep`.
//!
//! * A window registers its master and is SIGKILLed: the leader lives, the
//!   master turns Orphaned (after the death grace), the keeper relaunches once
//!   (a test command stands in for `open -a`), and the next window's HELLO is
//!   offered exactly that master — while the keeper still holds its copy. That
//!   window claims it, says BYE and exits 0: the keeper closes its copy and the
//!   leader is hung up, as today.
//! * NEGATIVE CONTROL: the same SIGKILL with the custody copy disabled (a
//!   library-only test seam) hangs the leader up at once.
//! * THE HOLDER SCAN: a window whose master is also held by a surviving
//!   process is SIGKILLed; nothing is offered while that process lives, and
//!   the master is offered once it is gone.
//! * THE CRASH MARKER (§5.4 row 3's cross-check): a window that holds a real
//!   locked marker, names it at HELLO, and quits through its exit path —
//!   `exit(0)`, the marker unlinked — WITHOUT a BYE has its shell hung up, as
//!   a quit; NEGATIVE CONTROL: the same end naming no marker is kept as an
//!   orphan (P3's lost-BYE resurrection). SIGKILLed, the marked window's
//!   marker stays with its lock free: an orphan.
//! * STALE ORPHANS: an orphan whose shell dies is closed on its leader's exit,
//!   with no HELLO, and the status no longer lists it.
//! * STREAM ORDER: the keeper is parked between two turns while the next
//!   window claims two offered masters, says BYE and exits 0, so its exit and
//!   the frames behind its first claim wait for the same turn; the keeper
//!   still reads them before the death, and the quit hangs both shells up.
//!   NEGATIVE CONTROL: the same end without the BYE keeps both as orphans.
//!
//! Every wait is on a causal event — a status line, a kernel exit status, a
//! line from a child — with a minute's hang detector, never a sleep. Every
//! process started here is ended by SIGKILL or by its own exit (AGENTS.md
//! rule 6).

#![cfg(target_vendor = "apple")]

use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use aterm_keeper::client::KeeperClient;
use aterm_keeper::identity::Identity;
use aterm_keeper::server::{Relauncher, ServeConfig, Server};
use aterm_keeper::wire::{Birth, Frame, MarkerRef, MasterHeader};
use aterm_uds::exitwatch::ExitWatch;

const ROLE: &str = "ATERM_KEEPER_TEST_ROLE";
const SOCK: &str = "ATERM_KEEPER_TEST_SOCK";
const MARK: &str = "ATERM_KEEPER_TEST_MARK";
const DROP: &str = "ATERM_KEEPER_TEST_DROP_CUSTODY";
/// The files a role reports through and is told to go on by: `<dir>/out-<n>`
/// and `<dir>/go-<n>` (libtest's child stdout is not a channel to rely on).
const TALK: &str = "ATERM_KEEPER_TEST_TALK";
/// The directory a marked window arms its crash marker in.
const LOGS: &str = "ATERM_KEEPER_TEST_LOGS";
const HANG: Duration = Duration::from_secs(60);

unsafe extern "C" {
    fn posix_openpt(flags: i32) -> i32;
    fn grantpt(fd: i32) -> i32;
    fn unlockpt(fd: i32) -> i32;
    fn ptsname(fd: i32) -> *const core::ffi::c_char;
    fn fcntl(fd: i32, cmd: i32, ...) -> i32;
    fn setsid() -> i32;
    fn ioctl(fd: i32, req: u64, ...) -> i32;
    fn dup2(a: i32, b: i32) -> i32;
    fn open(path: *const core::ffi::c_char, flags: i32, ...) -> i32;
    fn kill(pid: i32, sig: i32) -> i32;
    fn flock(fd: i32, op: i32) -> i32;
}

const O_RDWR: i32 = 2;
const O_NOCTTY: i32 = 0x20000;
const F_SETFD: i32 = 2;
const F_DUPFD: i32 = 0;
const FD_CLOEXEC: i32 = 1;
const TIOCSCTTY: u64 = 0x2000_7461;

/// Which role this process plays, when re-run by a test.
fn role() -> Option<String> {
    std::env::var(ROLE).ok()
}

fn sigkill(pid: u32) {
    // SAFETY: SIGKILL to a pid this test started.
    unsafe { kill(pid as i32, 9) };
}

// ---------------------------------------------------------------- the roles

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

/// The keeper: serve until killed.
fn keeper_role() -> ! {
    let mut server = Server::bind(keeper_config()).expect("bind");
    report("ready");
    let _ = server.run();
    std::process::exit(1)
}

/// The keeper, turn by turn: between two turns it parks when the test asks
/// (`<talk>.pause`), says so (`<talk>.paused`), runs no turn until told to go
/// on (`<talk>.resume`), and says when its first turn after that is done
/// (`<talk>.turned`). Serves until killed.
fn stepped_keeper_role() -> ! {
    let talk = PathBuf::from(std::env::var(TALK).expect("talk"));
    let note = |ext: &str| talk.with_extension(ext);
    let mut server = Server::bind(keeper_config()).expect("bind");
    report("ready");
    loop {
        let parked = note("pause").exists();
        if parked {
            std::fs::remove_file(note("pause")).expect("take the pause");
            std::fs::write(note("paused"), b"paused").expect("say parked");
            let started = Instant::now();
            while !note("resume").exists() {
                assert!(started.elapsed() < HANG * 5, "never resumed");
                std::thread::sleep(Duration::from_millis(5));
            }
            std::fs::remove_file(note("resume")).expect("take the resume");
        }
        if server.step(20).is_err() {
            std::process::exit(1)
        }
        if parked {
            std::fs::write(note("turned"), b"turned").expect("say turned");
        }
    }
}

/// A real PTY: the master (close-on-exec), and a `sleep` session leader whose
/// controlling terminal is the slave.
fn pty_with_leader() -> (OwnedFd, Child) {
    // SAFETY: plain libc calls on a descriptor this function owns.
    let master = unsafe { posix_openpt(O_RDWR | O_NOCTTY) };
    assert!(master >= 0, "posix_openpt");
    // SAFETY: as above.
    unsafe {
        assert_eq!(grantpt(master), 0);
        assert_eq!(unlockpt(master), 0);
        fcntl(master, F_SETFD, FD_CLOEXEC);
    }
    // SAFETY: ptsname returns a static buffer naming the slave; copied at once.
    let slave = unsafe { std::ffi::CStr::from_ptr(ptsname(master)) }.to_owned();
    // SAFETY: a fresh descriptor this function owns.
    let master = unsafe { OwnedFd::from_raw_fd(master) };
    let mut cmd = Command::new("/bin/sleep");
    cmd.arg("600");
    // SAFETY: only async-signal-safe calls between fork and exec.
    unsafe {
        use std::os::unix::process::CommandExt as _;
        cmd.pre_exec(move || {
            if setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            let fd = open(slave.as_ptr(), O_RDWR);
            if fd < 0 {
                return Err(std::io::Error::last_os_error());
            }
            if ioctl(fd, TIOCSCTTY, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            dup2(fd, 0);
            dup2(fd, 1);
            dup2(fd, 2);
            Ok(())
        });
    }
    let leader = cmd.spawn().expect("the session leader");
    (master, leader)
}

fn rdev_of(fd: &OwnedFd) -> u64 {
    use std::os::unix::fs::MetadataExt as _;
    let dup = fd.try_clone().expect("dup");
    std::fs::File::from(dup).metadata().expect("fstat").rdev()
}

/// A window: open a PTY with a leader, REGISTER a duplicate of the master
/// with the keeper, keep the primary, report, then wait. With
/// `ATERM_KEEPER_TEST_ROLE=window-holder`, also start a process that holds a
/// second duplicate of the master (an update successor's shape) and report it.
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
        // A non-close-on-exec duplicate at a high number, inherited by a
        // `sleep` that holds it for as long as it lives.
        // SAFETY: F_DUPFD on a descriptor we own; the result is inherited.
        let raw = unsafe { fcntl(master.as_raw_fd(), F_DUPFD, 300) };
        assert!(raw >= 300);
        let held = Command::new("/bin/sleep")
            .arg("600")
            .stdin(Stdio::null())
            .spawn()
            .expect("holder");
        // SAFETY: our own duplicate; the child has its copy.
        drop(unsafe { OwnedFd::from_raw_fd(raw) });
        line.push_str(&format!(" holder={}", held.id()));
        std::mem::forget(held);
    }
    report(&line);
    std::mem::forget(leader);
    // Wait to be killed; the primary master stays open until then.
    await_go();
    drop(master);
    std::process::exit(0)
}

/// A MARKED window: arm a crash marker the way `aterm-gui`'s `crash_signal`
/// does (created under a pending name, locked, renamed into place, the lock
/// held for life), name it at HELLO when `named`, open a PTY with a leader and
/// REGISTER it; on "go", end through the exit path — unlink the marker, then
/// `exit(0)` — WITHOUT a BYE: a quit whose BYE was lost.
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
    // SAFETY: a descriptor this role owns; LOCK_EX|LOCK_NB never waits.
    assert_eq!(
        unsafe { flock(file.as_raw_fd(), 2 | 4) },
        0,
        "the owner lock"
    );
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
    let offers = client.hello_app_marked("test", named_ref).expect("hello");
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
    // The exit path: the marker is unlinked, then exit(0). No BYE.
    std::fs::remove_file(&marker).expect("the exit path unlinks the marker");
    drop(master);
    std::process::exit(0)
}

/// The next window: HELLO, report the offers, wait for "go", then claim them
/// all, say BYE (unless `bye` is false: a lost BYE) and exit 0 (a quit).
fn next_window_role(bye: bool) -> ! {
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
        "offers={} rdevs={}",
        offers.len(),
        rdevs.join(",")
    ));
    await_go();
    for o in &offers {
        client
            .register(&o.master, o.header, o.tag.clone())
            .expect("claim");
    }
    if bye {
        client.send(&Frame::Bye, None).expect("bye");
    }
    // Stream order: the BYE is read before the EOF our exit makes.
    std::process::exit(0)
}

/// A role's one-line report to the test.
fn report(line: &str) {
    let talk = PathBuf::from(std::env::var(TALK).expect("talk"));
    let tmp = talk.with_extension("tmp");
    std::fs::write(&tmp, format!("{line}\n")).expect("report");
    std::fs::rename(&tmp, talk.with_extension("out")).expect("publish the report");
}

/// Wait for the test's "go" (a hang detector bounds it; a window waiting to be
/// killed waits the whole minute and is killed long before).
fn await_go() {
    let go = PathBuf::from(std::env::var(TALK).expect("talk")).with_extension("go");
    let started = Instant::now();
    while !go.exists() {
        assert!(started.elapsed() < HANG * 5, "never told to go");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn dispatch_role() {
    match role().as_deref() {
        Some("keeper") => keeper_role(),
        Some("keeper-stepped") => stepped_keeper_role(),
        Some("window") => window_role(false),
        Some("window-holder") => window_role(true),
        // Whether BYE is said is the role this child was launched as — read, not
        // a literal (aterm-spec's foreign_facts_are_observed).
        Some(role @ ("next-window" | "next-window-no-bye")) => {
            next_window_role(role == "next-window");
        }
        Some("marked-window") => marked_window_role(true),
        Some("unmarked-window") => marked_window_role(false),
        _ => {}
    }
}

// ---------------------------------------------------------------- the tests

fn born(pid: u32) -> Birth {
    aterm_keeper::server::born(pid).expect("a live process has a birth record")
}

struct Scratch {
    dir: PathBuf,
    kids: Vec<Child>,
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
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

impl Scratch {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("akk-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("logs")).expect("scratch");
        Scratch {
            dir,
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
    /// Re-run this test binary as `role`, stdin and stdout piped.
    fn spawn(&mut self, test: &str, role: &str, extra: &[(&str, &str)]) -> usize {
        let mut cmd = Command::new(std::env::current_exe().expect("test binary"));
        cmd.arg(test)
            .arg("--exact")
            .arg("--nocapture")
            .arg("--test-threads=1")
            .env(ROLE, role)
            .env(SOCK, self.sock())
            .env(MARK, self.mark())
            .env(TALK, self.dir.join(format!("kid-{}", self.kids.len())))
            .env(LOGS, self.dir.join("logs"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        for (k, v) in extra {
            cmd.env(k, v);
        }
        self.kids.push(cmd.spawn().expect("spawn a role"));
        self.kids.len() - 1
    }
    /// A child's report, once it has made one (a hang detector bounds it).
    fn line(&mut self, kid: usize, prefix: &str) -> String {
        let out = self.dir.join(format!("kid-{kid}.out"));
        let started = Instant::now();
        loop {
            if let Ok(text) = std::fs::read_to_string(&out) {
                let line = text.trim().to_string();
                assert!(
                    line.starts_with(prefix),
                    "{prefix:?} expected, got {line:?}"
                );
                return line;
            }
            if let Ok(Some(status)) = self.kids[kid].try_wait() {
                panic!("the child ended ({status}) before reporting {prefix:?}");
            }
            assert!(started.elapsed() < HANG, "no report {prefix:?}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    fn say(&mut self, kid: usize, _what: &str) {
        std::fs::write(self.dir.join(format!("kid-{kid}.go")), b"go").expect("go");
    }
    /// Park the stepped keeper `kid` between two of its turns: returns once it
    /// has finished its turn and runs none until [`Self::resume`].
    fn pause(&mut self, kid: usize) {
        std::fs::write(self.dir.join(format!("kid-{kid}.pause")), b"pause").expect("pause");
        self.await_note(kid, "paused");
    }
    /// Let the parked keeper `kid` go on; returns once it has run one turn,
    /// with no connection of this test open during it (the keeper's holder
    /// scan reads every peer's descriptor table, and a peer whose table is
    /// churning — this multi-threaded test process — reads as "unknown").
    fn resume(&mut self, kid: usize) {
        std::fs::write(self.dir.join(format!("kid-{kid}.resume")), b"resume").expect("resume");
        self.await_note(kid, "turned");
    }
    fn await_note(&mut self, kid: usize, what: &str) {
        let note = self.dir.join(format!("kid-{kid}.{what}"));
        let started = Instant::now();
        while !note.exists() {
            if let Ok(Some(status)) = self.kids[kid].try_wait() {
                panic!("the keeper ended ({status}) before it {what}");
            }
            assert!(started.elapsed() < HANG, "the keeper never {what}");
            std::thread::sleep(Duration::from_millis(5));
        }
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
            .and_then(|c| c.status())
            .expect("status")
    }
    /// Poll the keeper's status until `pred` holds (a hang detector bounds it).
    fn await_status(&self, what: &str, pred: impl Fn(&str) -> bool) -> String {
        let started = Instant::now();
        loop {
            let s = self.status();
            if pred(&s) {
                return s;
            }
            assert!(started.elapsed() < HANG, "never {what}:\n{s}");
            std::thread::yield_now();
        }
    }
}

fn field(line: &str, key: &str) -> u64 {
    line.split_whitespace()
        .find_map(|kv| kv.strip_prefix(&format!("{key}=")))
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(|| panic!("{key} in {line:?}"))
}

/// Wait for a watched process's kernel status (a hang detector bounds it).
fn await_exit(watch: &ExitWatch) -> i32 {
    let started = Instant::now();
    loop {
        if let Some(s) = watch.exit_status() {
            return s;
        }
        assert!(started.elapsed() < HANG, "pid {} never exited", watch.pid());
        std::thread::yield_now();
    }
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

    // The window is SIGKILLed.
    let win_pid = s.kids[w].id();
    sigkill(win_pid);
    let _ = s.kids[w].wait();
    let st = s.await_status("orphaned or relaunched", |t| {
        t.contains("orphaned=1") || t.contains("state=launching")
    });
    assert_eq!(leader_watch.exit_status(), None, "the leader lives:\n{st}");
    assert!(aterm_uds::process::pid_alive(leader));
    // One relaunch, by the test command, before any window answered.
    let started = Instant::now();
    while relaunch_lines(&s) < 1 {
        assert!(started.elapsed() < HANG, "the keeper never relaunched");
        std::thread::yield_now();
    }

    // The next window is offered exactly that master, and the keeper keeps its copy.
    let next = s.spawn(T_CRASH, "next-window", &[]);
    let got = s.line(next, "offers=");
    assert_eq!(
        got,
        format!("offers=1 rdevs={rdev}"),
        "exactly one OFFER, for that rdev"
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

    // The next window claims it, says BYE and exits 0: a quit hangs the shell up.
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

const T_CONTROL: &str = "without_the_custody_copy_a_killed_window_hangs_its_shell_up";

/// NEGATIVE CONTROL: the keeper runs, the window registers, but the custody
/// copy is closed at once (the test seam): the SIGKILL hangs the leader up.
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

/// THE HOLDER SCAN: the killed window's master is still held by a live
/// process (the shape of an update successor), so it is held, not offered;
/// once that process is gone, it is offered.
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
    // A window now is offered nothing.
    let early = s.spawn(T_HOLDER, "next-window", &[]);
    assert_eq!(s.line(early, "offers="), "offers=0 rdevs=");
    s.say(early, "go");
    let _ = s.kids[early].wait();
    assert_eq!(relaunch_lines(&s), 0, "no relaunch over a live holder");
    // The holder goes: the master is an orphan, and offered.
    let holder_watch = ExitWatch::watch(holder).expect("watch the holder");
    sigkill(holder);
    await_exit(&holder_watch);
    s.await_status("orphaned", |t| {
        t.contains("orphaned=1") || t.contains("state=launching")
    });
    let next = s.spawn(T_HOLDER, "next-window", &[]);
    assert_eq!(s.line(next, "offers="), format!("offers=1 rdevs={rdev}"));
    assert_eq!(
        leader_watch.exit_status(),
        None,
        "the leader lived throughout"
    );
    s.say(next, "go");
    let _ = s.kids[next].wait();
    assert_eq!(await_exit(&leader_watch), 1, "and the quit hung it up");
}

const T_LOST_BYE: &str = "a_quit_whose_bye_was_lost_hangs_its_shell_up";

/// ROW 3's CROSS-CHECK on a real kernel: a window names its locked marker,
/// then quits through its exit path — the marker unlinked, `exit(0)` — with
/// no BYE. The keeper reads the status and the marker's absence as the quit
/// it was: its copy closes and the leader is hung up, as today.
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

/// NEGATIVE CONTROL: the same end — `exit(0)`, no BYE — from a window that
/// named no marker is P3's lost BYE: the keeper cannot tell it from a crash,
/// and keeps the shell as an orphan.
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

/// A marked window SIGKILLed leaves its marker with the lock free: a crash,
/// and the shell an orphan. Then STALE ORPHANS: the leader is SIGKILLed, and
/// the keeper — which watches every orphan's leader — closes the record at
/// once, with no HELLO; the status lists no master.
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
    // The leader dies: pruned at once.
    sigkill(leader);
    assert_eq!(await_exit(&leader_watch), 9);
    let st = s.await_status("pruned", |t| t.contains("pruned=1"));
    assert!(st.contains("masters=0"), "{st}");
    assert!(
        !st.contains(&format!("shell={leader}")),
        "never listed dead:\n{st}"
    );
}

/// What [`an_exit_that_overtakes_its_frames`] leaves.
struct Overtaken {
    /// The keeper's status once the next window's end is judged.
    status: String,
    /// The status's `frames_behind_exit`.
    behind: u64,
    /// The crashes judged at the next window's end.
    crashes: u64,
    /// The two leaders' exit watches.
    leaders: Vec<ExitWatch>,
    rdevs: Vec<u64>,
    keeper_pid: u32,
}

/// STREAM ORDER on a real kernel: two windows, one master each, are
/// SIGKILLed, and the next window is offered both. With the keeper parked
/// between two turns, that window claims both, says BYE (when `bye`) and
/// exits 0, so its exit and every frame it wrote wait for the same turn. The
/// turn reads the first claim (one frame per connection), then the exit; the
/// claim and the BYE still behind it must be read before the death is
/// recorded — a claim read after it is refused, and its master goes back to
/// Orphaned with the keeper's copy kept and its shell never hung up (the
/// 2026-09-29 gate flake, in which the keeper was the master's only holder).
fn an_exit_that_overtakes_its_frames(s: &mut Scratch, test: &str, bye: bool) -> Overtaken {
    let keeper = s.spawn(test, "keeper-stepped", &[]);
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
    let role = if bye {
        "next-window"
    } else {
        "next-window-no-bye"
    };
    let next = s.spawn(test, role, &[]);
    let got = s.line(next, "offers=");
    assert!(got.starts_with("offers=2 "), "both masters offered: {got}");
    let st = s.await_status("offered", |t| t.contains("offered=2"));
    for w in &leaders {
        assert_eq!(w.exit_status(), None, "the leaders live:\n{st}");
    }
    let crashes_before = ends(&st, "crash");

    s.pause(keeper);
    s.say(next, "go");
    let ended = s.kids[next].wait().expect("the next window's status");
    assert!(ended.success(), "the next window exited 0: {ended}");
    s.resume(keeper);
    let status = s.await_status("the window's end judged", |t| {
        t.contains("claimed=0") && t.contains("offered=0") && t.contains("windows=0")
    });
    let behind = status
        .lines()
        .find_map(|l| l.strip_prefix("frames_behind_exit="))
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(|| panic!("frames_behind_exit in\n{status}"));
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

/// Every process of this uid that holds each master, by the kernel's
/// descriptor tables (a failure message's evidence).
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

/// One count of the status's `ends` line.
fn ends(status: &str, key: &str) -> u64 {
    let line = status
        .lines()
        .find(|l| l.starts_with("ends "))
        .unwrap_or_else(|| panic!("an ends line in\n{status}"));
    field(line, key)
}

const T_OVERTAKE: &str = "a_quit_whose_exit_overtakes_its_claims_hangs_both_shells_up";

#[test]
fn a_quit_whose_exit_overtakes_its_claims_hangs_both_shells_up() {
    dispatch_role();
    let mut s = Scratch::new("overtake");
    let o = an_exit_that_overtakes_its_frames(&mut s, T_OVERTAKE, true);
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
    // Not vacuous: the second claim and the BYE were behind the exit.
    assert!(o.behind >= 2, "two frames read behind the exit:\n{st}");
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

/// NEGATIVE CONTROL: the same overtaken end without the BYE is a lost BYE —
/// both claims are read and judged a crash, and both shells are kept as
/// orphans: the hang-up above is the quit's, not the window's exit's.
#[test]
fn without_a_bye_an_overtaken_claim_keeps_both_shells() {
    dispatch_role();
    let mut s = Scratch::new("overtake-ctl");
    let o = an_exit_that_overtakes_its_frames(&mut s, T_OVERTAKE_CONTROL, false);
    let st = &o.status;
    assert!(st.contains("orphaned=2"), "{st}");
    assert!(st.contains("custody_copies=2"), "{st}");
    assert_eq!(o.crashes, 2, "both claims read, then judged a crash:\n{st}");
    assert!(
        o.behind >= 1,
        "the second claim read behind the exit:\n{st}"
    );
    for w in &o.leaders {
        assert_eq!(w.exit_status(), None, "the leader lives:\n{st}");
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
