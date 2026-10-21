// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Real-kernel integration tests for `aterm-keeper` (`docs/DESIGN-pty-keeper-2026-09-26.md` §6.4).
//!
//! Spawns this test binary in keeper, window, and successor roles over real
//! PTY pairs (`SCM_RIGHTS`) with `/bin/sleep` session leaders, exercising
//! orphan recovery, holder scanning, unplaced-offer release, crash markers,
//! stale-orphan pruning, stream ordering across process exit, dead-peer
//! holder filtering, and POSIX shared-memory crash-restore journals.

#![cfg(target_vendor = "apple")]
#![allow(
    clippy::disallowed_types,
    reason = "test code: a stand-in child may inherit descriptors"
)]

use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use aterm_keeper::client::{KeeperClient, KeeperLink, LinkOp};
use aterm_keeper::identity::Identity;
use aterm_keeper::server::{Relauncher, ServeConfig, Server};
use aterm_keeper::wire::{Birth, Frame, MarkerRef, MasterHeader};
use aterm_spec::wait::{MUST_HAPPEN, within};
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

/// Which role this process plays, when re-run by a test.
fn role() -> Option<String> {
    std::env::var(ROLE).ok()
}

fn sigkill(pid: u32) {
    // SAFETY: SIGKILL to a pid this test started.
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
            let resumed = within(HANG * 5, Duration::from_millis(5), || {
                note("resume").exists().then_some(())
            });
            assert!(resumed.is_some(), "never resumed");
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

/// A real PTY: the master, and a `sleep` session leader whose controlling
/// terminal is the slave. The pair is aterm-pty's `open_pty_pair`: both ends
/// close-on-exec from the open, not set after it (a sibling test that forks in
/// between would otherwise inherit one), and a `/dev/ptmx` open that lost
/// XNU's race for a pty minor to another process's (`EREDRIVEOPEN`, -6) is
/// redriven rather than failed.
fn pty_with_leader() -> (OwnedFd, Child) {
    use aterm_uds::spawnfd::InheritNothing as _;
    let (master, slave) = aterm_pty::open_pty_pair().expect("open_pty_pair");
    // SAFETY: two fresh descriptors this function alone owns.
    let (master, slave) = unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
    let stdio = || Stdio::from(slave.try_clone().expect("dup the slave"));
    let mut cmd = Command::new("/bin/sleep");
    // std's, for the hook below, with the strip installed first: the leader
    // holds its terminal and nothing else of this process.
    cmd.inherit_nothing()
        .arg("600")
        .stdin(stdio())
        .stdout(stdio())
        .stderr(stdio());
    // SAFETY: only async-signal-safe calls between fork and exec, on the
    // child's own stdin, which is the slave by then.
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

/// A window: open a PTY with a leader, REGISTER a duplicate of the master
/// with the keeper, keep the primary, report, then wait. With
/// `ATERM_KEEPER_TEST_ROLE=window-holder`, also start a process that holds a
/// second duplicate of the master (an update successor's shape) and report it.
/// Told "bye", it says BYE and exits 0 (a quit); told "poke", it writes one
/// frame that earns no answer, says so (`<talk>.poked`) and waits to be
/// killed; told anything else, it exits 0.
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
        // Duplicate below macOS default soft `RLIMIT_NOFILE` (256) inherited
        // via `spawnfd::transparent` by a child `sleep` holder.
        // SAFETY: F_DUPFD on a descriptor we own; the result is inherited.
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
        // SAFETY: our own duplicate; the child has its copy.
        drop(unsafe { OwnedFd::from_raw_fd(raw) });
        line.push_str(&format!(" holder={}", held.id()));
        std::mem::forget(held);
    }
    report(&line);
    std::mem::forget(leader);
    // Wait to be killed or told; the primary master stays open until then.
    match await_go().as_str() {
        "bye" => client.send(&Frame::Bye, None).expect("bye"),
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
    // `flock(LOCK_EX|LOCK_NB)`, as `create_locked` takes it.
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
    // The exit path: the marker is unlinked, then exit(0). No BYE.
    std::fs::remove_file(&marker).expect("the exit path unlinks the marker");
    drop(master);
    std::process::exit(0)
}

/// The next window: HELLO, report the offers, wait for "go", then claim them
/// all, say BYE (unless `bye` is false: a lost BYE) and exit 0 (a quit). With
/// `adopt`, an ADOPT follows the first claim — a frame the keeper answers
/// (WELCOME), and this window never reads the answer: it has exited by then.
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
        "offers={} rdevs={}",
        offers.len(),
        rdevs.join(",")
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
        client.send(&Frame::Bye, None).expect("bye");
    }
    // Stream order: the BYE is read before the EOF our exit makes.
    std::process::exit(0)
}

/// Legacy window role (`HELLO` without `CAP_JOURNAL`): reads raw frames to
/// verify `OFFER`s omit journal fields and no `COMPANION_OFFER` is sent,
/// then claims every master on cue so the keeper closes the journal region.
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
    // One report (each replaces the last): the frames themselves ride along only when there are
    // strays, so the test's comparison shows them.
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
    client.send(&Frame::Bye, None).expect("bye");
    std::process::exit(0)
}

/// Wait until the test says exactly `what` (a role told several things in turn; a hang detector
/// bounds it).
fn await_said(what: &str) {
    let go = PathBuf::from(std::env::var(TALK).expect("talk")).with_extension("go");
    within(HANG, Duration::from_millis(5), || {
        (std::fs::read_to_string(&go).ok()? == what).then_some(())
    })
    .unwrap_or_else(|| panic!("never told {what:?}"));
}

/// A recovering window that could not place what it was offered: HELLO, drop
/// every offered master, start its link on the boot connection, RELEASE each
/// offer through the link (`aterm-gui`'s `keeper_link::Claim`, dropped
/// undischarged), say BYE and exit 0 (a quit).
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
    assert!(link.send_and_wait(LinkOp::Bye, HANG), "the BYE is written");
    report(&format!("released={}", rdevs.len()));
    std::process::exit(0)
}

/// Outgoing update window role whose `quiet-successor` holds a master
/// duplicate without dialing the keeper: registers a PTY, names the
/// successor in `PENDING`, and exits 0 without `BYE` on cue.
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
    // The successor's duplicate: not close-on-exec, at a number under the
    // default soft `RLIMIT_NOFILE` (as `window_role`'s holder).
    // SAFETY: F_DUPFD on a descriptor we own; the result is inherited.
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
    // Clear `FD_CLOEXEC` on `raw` in the child after Trust's SP-1 `pre_exec` pass.
    // SAFETY: one async-signal-safe `fcntl` between fork and exec, on a
    // descriptor the child holds from the fork.
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
    // SAFETY: our own duplicate; the successor has its copy.
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

/// The successor of [`outgoing_window_role`]: it never dials the keeper; on
/// "go" it quits, `exit(0)`.
fn quiet_successor_role() -> ! {
    await_go();
    std::process::exit(0)
}

/// A role's one-line report to the test.
fn report(line: &str) {
    let talk = PathBuf::from(std::env::var(TALK).expect("talk"));
    let tmp = talk.with_extension("tmp");
    std::fs::write(&tmp, format!("{line}\n")).expect("report");
    std::fs::rename(&tmp, talk.with_extension("out")).expect("publish the report");
}

/// A role's note to the test that something happened (`<talk>.<what>`).
fn note(what: &str) {
    let talk = PathBuf::from(std::env::var(TALK).expect("talk"));
    std::fs::write(talk.with_extension(what), what).expect("note");
}

/// Wait for the test's "go" and return what it said (a hang detector bounds
/// it; a window waiting to be killed waits the whole minute and is killed long
/// before).
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
        Some("keeper-stepped") => stepped_keeper_role(),
        Some("window") => window_role(false),
        Some("window-holder") => window_role(true),
        // Whether BYE and ADOPT are said is the role this child was launched
        // as — read, not a literal.
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
        // A relaunch the keeper fired (`echo relaunch >> <dir>/relaunches`,
        // [`keeper_config`]) is no child of this test, and its line can land
        // while the tree is being removed, so the removal fails as not empty.
        // One line per relaunch: retry until the directory is gone.
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
    /// Re-run this test binary as `role`, its stdio `/dev/null`.
    fn spawn(&mut self, test: &str, role: &str, extra: &[(&str, &str)]) -> usize {
        let mut cmd = aterm_uds::spawnfd::command(std::env::current_exe().expect("test binary"));
        cmd.arg(test)
            .arg("--exact")
            .arg("--nocapture")
            .arg("--test-threads=1")
            .env(ROLE, role)
            .env(SOCK, self.sock())
            .env(MARK, self.mark())
            .env(TALK, self.dir.join(format!("kid-{}", self.kids.len())))
            .env(LOGS, self.dir.join("logs"))
            .stdin(aterm_uds::spawnfd::Stdio::null())
            .stdout(aterm_uds::spawnfd::Stdio::null())
            .stderr(aterm_uds::spawnfd::Stdio::null());
        for (k, v) in extra {
            cmd.env(k, v);
        }
        self.kids.push(cmd.spawn().expect("spawn a role"));
        self.kids.len() - 1
    }
    /// A child's report, once it has made one (a hang detector bounds it).
    fn line(&mut self, kid: usize, prefix: &str) -> String {
        let out = self.dir.join(format!("kid-{kid}.out"));
        let text = within(HANG, Duration::from_millis(5), || {
            if let Ok(text) = std::fs::read_to_string(&out) {
                return Some(text);
            }
            if let Ok(Some(status)) = self.kids[kid].try_wait() {
                // A role may report and exit between the read above and this
                // look (`unplaced-window` does both at once). Its report is
                // published before its exit, so it is there now if ever.
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
    /// Tell the role `kid` to go on; `what` is what it is told (published
    /// whole, by a rename).
    fn say(&mut self, kid: usize, what: &str) {
        self.tell(&format!("kid-{kid}"), what);
    }
    /// [`Self::say`] to the role whose talk file is `<dir>/<talk>`.
    fn tell(&self, talk: &str, what: &str) {
        let tmp = self.dir.join(format!("{talk}.say"));
        std::fs::write(&tmp, what).expect("say");
        std::fs::rename(&tmp, self.dir.join(format!("{talk}.go"))).expect("go");
    }
    /// Park the stepped keeper `kid` between two of its turns: returns once it
    /// has finished its turn and runs none until [`Self::resume`].
    fn pause(&mut self, kid: usize) {
        std::fs::write(self.dir.join(format!("kid-{kid}.pause")), b"pause").expect("pause");
        self.await_note(kid, "paused");
    }
    /// Let the parked keeper `kid` go on; returns once it has run one turn,
    /// with no connection of this test open during it, so the turn's holder
    /// scans meet only the roles as peers. This multi-threaded test process's
    /// churning descriptor table does not read as "unknown": a descriptor
    /// closed mid-scan is not held (`aterm_uds::holders`).
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
    /// Poll the keeper's status until `pred` holds (a hang detector bounds it).
    fn await_status(&self, what: &str, pred: impl Fn(&str) -> bool) -> String {
        let mut last = String::new();
        let hit = within(HANG, Duration::ZERO, || {
            last = self.status();
            pred(&last).then(|| last.clone())
        });
        hit.unwrap_or_else(|| panic!("never {what}:\n{last}"))
    }
}

/// The crash-restore regions the keeper process really holds: the sizes of its POSIX shared
/// memory descriptors, read from the kernel's table of the keeper (named by its status's `pid=`),
/// never from what its status says it holds. The loop closes a descriptor before it answers the
/// next status, so a status that shows the change already has the table to match.
fn keeper_regions(s: &Scratch) -> Vec<u64> {
    let pid = u32::try_from(field(&s.status(), "pid")).expect("a pid");
    aterm_uds::holders::pshm_sizes_of(pid).expect("the keeper's descriptor table")
}

/// A journal region's size, as [`keeper_regions`] lists it.
const REGION: u64 = aterm_restore::REGION_LEN as u64;

fn field(line: &str, key: &str) -> u64 {
    line.split_whitespace()
        .find_map(|kv| kv.strip_prefix(&format!("{key}=")))
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(|| panic!("{key} in {line:?}"))
}

/// Wait for a watched process's kernel status (a hang detector bounds it).
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
    let relaunched = within(HANG, Duration::ZERO, || {
        (relaunch_lines(&s) >= 1).then_some(())
    });
    assert!(relaunched.is_some(), "the keeper never relaunched");

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

const T_UNPLACED: &str = "an_offer_the_window_could_not_place_is_released_through_its_link";

/// When a recovering window `RELEASE`s an offered master it could not place
/// and quits, the keeper closes its custody copy and hangs up the shell.
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

/// When an outgoing window names a `PENDING` successor that never dials the
/// keeper and later exits 0, the keeper treats the clean exit as a quit.
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

/// Park the keeper between turns while the next window claims two offered
/// masters, optionally sends `ADOPT` and `BYE`, and exits 0, verifying
/// queued frames behind the first claim are processed before process death.
fn an_exit_that_overtakes_its_frames(s: &mut Scratch, test: &str, role: &str) -> Overtaken {
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
    let o = an_exit_that_overtakes_its_frames(&mut s, T_OVERTAKE, "next-window");
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
    let o = an_exit_that_overtakes_its_frames(&mut s, T_OVERTAKE_CONTROL, "next-window-no-bye");
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

/// One `key=value` line of the status.
fn status_count(status: &str, key: &str) -> u64 {
    status
        .lines()
        .find_map(|l| l.strip_prefix(&format!("{key}=")))
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(|| panic!("{key} in\n{status}"))
}

const T_REPLY: &str = "a_reply_the_dead_window_cannot_read_loses_none_of_its_frames";

/// A REPLY TO THE DEAD: the overtaken end again, with an ADOPT after the first
/// claim. The keeper reads the ADOPT after the window has died, and its
/// WELCOME cannot be written; that failure must end only the keeper's writing,
/// never its reading — the second claim and the BYE behind the ADOPT are still
/// read, and the quit hangs both shells up.
#[test]
fn a_reply_the_dead_window_cannot_read_loses_none_of_its_frames() {
    dispatch_role();
    let mut s = Scratch::new("reply");
    let o = an_exit_that_overtakes_its_frames(&mut s, T_REPLY, "next-window-adopt");
    let st = &o.status;
    assert!(
        st.contains("masters=0"),
        "every frame behind the unwritable reply read, then the quit:\n{st}"
    );
    assert!(st.contains("custody_copies=0"), "{st}");
    assert_eq!(ends(st, "quit"), 2, "{st}");
    assert_eq!(o.crashes, 0, "{st}");
    // Not vacuous: the reply to the dead window was not written.
    assert!(status_count(st, "replies_lost") >= 1, "{st}");
    for w in &o.leaders {
        assert_eq!(await_exit(w), 1, "the leader was hung up (SIGHUP)");
    }
}

const T_REPLY_CONTROL: &str = "without_a_bye_a_reply_lost_to_the_dead_keeps_both_shells";

/// NEGATIVE CONTROL: the same unwritable reply with no BYE behind it — both
/// claims are still read (both judged a crash, not one), and both shells are
/// kept: the hang-up above is the BYE's, read behind the reply.
#[test]
fn without_a_bye_a_reply_lost_to_the_dead_keeps_both_shells() {
    dispatch_role();
    let mut s = Scratch::new("reply-ctl");
    let o = an_exit_that_overtakes_its_frames(&mut s, T_REPLY_CONTROL, "next-window-adopt-no-bye");
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

/// When one window is `SIGKILL`ed (unreaped zombie) and another sends `BYE`
/// and exits 0 in the same parked turn, the holder scan treats the dead
/// peer's descriptor table as empty so the quitting window's shell hangs up.
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
    // The keeper reads exits in pid order: the quitter's is read first.
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

/// The journaled session's screen: small, so the oracle's text is readable in a failure.
const JOURNAL_ROWS: u16 = 12;
const JOURNAL_COLS: u16 = 48;
/// The engine config fingerprint both the writer and the restorer run under.
const JOURNAL_CONFIG: u64 = 0x0a7e_c0de;
/// The slice of [`journal_output`] at which the window takes its second keyframe: what comes
/// after it lives only in the region's tail, so an exact restore must replay it, and a restore
/// of the keyframe alone shows the screen as of here.
const KEYFRAMED: usize = 20;

/// What the dead window's session printed, slice by slice: colour, wide UTF-8, and more lines
/// than the screen holds.
fn journal_output() -> Vec<Vec<u8>> {
    let mut out: Vec<Vec<u8>> = (0..40)
        .map(|i| format!("\x1b[3{}mline {i:03} of the dead window\x1b[0m\r\n", i % 8).into_bytes())
        .collect();
    out.push("$ \u{2713} last words \u{4e16}\u{754c}".as_bytes().to_vec());
    out
}

/// The oracle: the screen text an engine shows after `slices`.
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

/// Write a keyframe of `term` through `writer`.
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

/// A window that JOURNALS: its HELLO states CAP_JOURNAL, it registers a PTY's master with a
/// leader, then (per `journaled`) the region it journaled the session into. Told "die", it dies
/// by its own SIGKILL, mid-life, as a crash does; told anything else, it says BYE and exits 0.
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
    // Kept alive until the death: the window's own mapping and descriptors die with it.
    let mut kept = Vec::new();
    let mut writer = None;
    match journaled {
        Journaled::Region => {
            #[expect(
                clippy::disallowed_methods,
                reason = "a test playing the window makes the region the keeper is sent"
            )]
            // SAFETY: only this fixture's JournalWriter changes the object; the keeper
            // only holds/queries custody. Restorers map it only after the parent reaps this
            // writer process; no participant reopens custody for writes or resizing.
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
        _ => client.send(&Frame::Bye, None).expect("bye"),
    }
    drop((writer, kept, master));
    std::process::exit(0)
}

/// The next window that JOURNALS: HELLO stating CAP_JOURNAL, then for each offer report whether
/// a region came, the brake, and what the region restores (`restore=exact|screen|meta`), the
/// restored screen written to `<talk>.screen`. Told "die-claimed", it claims every master and
/// dies by its own SIGKILL before any COMPANION (a restorer the region killed); told anything
/// else, it claims every master, says it has no region of its own (COMPANION_RELEASE), says BYE
/// and exits 0.
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
        // SAFETY: every fixture starts this role only after journal_window_dies has reaped
        // the publishing process. Only read-only custody survives, held here throughout
        // recovery; no surviving writer or resizing descriptor can alter the private object.
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
    client.send(&Frame::Bye, None).expect("bye");
    std::process::exit(0)
}

/// Whether a role's end was its own SIGKILL.
fn killed(status: std::process::ExitStatus) -> bool {
    use std::os::unix::process::ExitStatusExt as _;
    status.signal() == Some(9)
}

/// The journaling window `w` is told to die, dies by SIGKILL, and its master is orphaned.
fn journal_window_dies(s: &mut Scratch, w: usize) {
    s.say(w, "die");
    let ended = s.kids[w].wait().expect("its status");
    assert!(killed(ended), "the window died by SIGKILL: {ended}");
    s.await_status("orphaned", |t| {
        t.contains("orphaned=1") || t.contains("state=launching")
    });
}

/// A journaling window starts and registers, its custody shown in the status as `journals`:
/// the role's index, its leader and its rdev.
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

/// THE LANE: the region a SIGKILLed window journaled into is offered with its master, and the
/// next window restores exactly the screen the dead one showed, its tail replayed; the region
/// closes with the master when that window quits. What the keeper holds is read from the keeper
/// process's descriptor table at each step, not from its status.
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

/// NEGATIVE CONTROL: a window that journals but registered no region dies; the next window is
/// offered the master with no region, the META rung (and the unsettled claim's death counts an
/// attempt, as the model's `ReaderDies` does).
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

/// NEGATIVE CONTROL: the first restorer claims the master and dies before its COMPANION (as a
/// window whose replay killed it would); the keeper's next offer says `attempts=1`, and the
/// region then restores its keyframe alone: the screen as of the keyframe, never called exact.
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

/// NEGATIVE CONTROL (§3.2 J2, N5): a client without `CAP_JOURNAL` receives
/// plain `OFFER`s with no `COMPANION_OFFER`, and its `CLAIM` closes the
/// keeper's journal descriptor.
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

/// NEGATIVE CONTROL (§3.2 N8): a custody descriptor open for writing is refused as no journal
/// (any window offered it could map the dead window's words writable); the read-only one the
/// same window sends next is held, and only it, as the keeper's descriptor table shows.
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
