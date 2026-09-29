// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The window's control socket keeps answering new clients however many
//! PERSISTENT drivers hold connections to it, across a real headless instance.
//!
//! THE DEFECT (the live end-to-end run of 2026-09-26): the socket served a
//! connection on one of eight fixed lanes for as long as the connection stayed
//! open, so an open connection cost a lane whether it had a request or not —
//! and every supervised agent tab holds one (its loop's persistent
//! `RelayCtl`), parked most of the time in a 20 s `await`. Six supervised tabs
//! plus two held connections made every new `aterm ctl` fail for a minute; with
//! eight supervised tabs every other client — the owner's `aterm ctl`, agents,
//! the fabric — would have been refused outright
//! (`ERR control server busy; retry`).
//!
//! What this drives, over the real socket of the one `aterm` binary:
//!
//! * `idle_and_waiting_drivers_do_not_starve_a_new_client` — twelve
//!   authenticated connections left IDLE between requests and twelve more
//!   parked in a long `await` — twenty-four persistent drivers, three times
//!   the eight lanes — and then fresh clients, raw and through `aterm ctl`,
//!   must be answered every time. The held connections still answer in order
//!   afterwards (the persistent-driver contract is kept, not traded away).
//! * `a_pool_saturated_with_waits_still_answers_busy`, the NEGATIVE CONTROL:
//!   the busy reply is not gone, it is reserved for a socket truly saturated
//!   with requests. Waits are opened until a fresh client is refused; that
//!   must happen, promptly and in the documented words, only once every wait
//!   lane AND every request lane holds a wait (idle drivers held alongside do
//!   not count), and the socket must recover once the waits end.
//! * `the_connection_past_the_cap_is_told_busy_and_tabs_still_open`: the
//!   open-connection bound answers to the descriptor limit. Under launchd's
//!   default (a soft `RLIMIT_NOFILE` of 256, what the window started from the
//!   Dock runs with) idle drivers are opened until a refusal STAYS (a
//!   momentary one, while the lanes are still placing earlier connections, is
//!   dialled again, and one is made on purpose so the retry is exercised): the
//!   refusal must be the busy line, at exactly a quarter of the limit, and the
//!   window must still open tabs. A fixed 1024 let about
//!   240 idle connections use the process up (2026-09-26): every fresh client
//!   then got a dropped connection instead of the busy line, and a new tab had
//!   no descriptor left.
//! * `hung_up_waits_give_their_lanes_back_and_ctl_names_busy`: a blocking verb
//!   whose caller hangs up gives its lane back within a hangup poll, not at its
//!   own timeout (2026-09-25: SIGKILLed `aterm ctl await … timeout=20000`
//!   clients kept every lane busy for the full 20 s). The socket is saturated
//!   with long waits; while it is, `aterm ctl` reports the refusal as busy — an
//!   ordinary failure naming the server's words, never "Broken pipe"; then
//!   every waiting client hangs up and a fresh client, and a fresh wait, must be
//!   admitted within a second and a half — long before the waits' own minute.
//!   The listener's health fields are on `metrics`.
//!
//! Every instance runs under a descriptor limit its test names (the cap is a
//! quarter of it), so no verdict depends on the shell the suite was started
//! from.
//!
//! ISOLATION: scratch HOME/XDG roots, a private explicit control socket, a
//! config with every automatic lane off (the harness included), `--no-reroute`,
//! `SHELL=/bin/sh` (`support/launch_isolation.rs`). SKIP (not fail) when an
//! instance cannot boot.

#![cfg(unix)]

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

#[path = "support/launch_isolation.rs"]
mod launch_isolation;

const MAX_SOCK_PATH: usize = 100;

/// The server's request lanes (`CONTROL_WORKERS` in `aterm-gui/src/control.rs`)
/// and its wait lanes (`CONTROL_WAIT_WORKERS`): the saturation point the
/// negative control expects, spelled here because the wire does not say it.
const RPC_LANES: usize = 8;
const WAIT_LANES: usize = 64;

/// The busy reply, word for word (`control.rs`'s listener).
const BUSY: &str = "ERR control server busy; retry";

/// How long a refusal must last to be read as the socket's verdict rather than
/// a moment: a lane places a connection in milliseconds, and a cap or a
/// saturation lasts until a connection closes or a wait ends.
const REFUSAL_STAYS: Duration = Duration::from_millis(500);

/// Where the cap test makes a refusal momentary on purpose: well below the cap
/// (a quarter of launchd's 256 is 64).
const MOMENT_AT: usize = 16;

/// The soft descriptor limit launchd gives an app it starts (the Dock, Finder,
/// `open`): the window's own budget, which aterm does not raise.
const LAUNCHD_SOFT_LIMIT: u64 = 256;

/// A limit roomy enough that the connection cap (a quarter of it, at most
/// 1024) is never what a test about the LANES runs into.
const ROOMY_SOFT_LIMIT: u64 = 4_096;

/// The open-connection cap the server derives from a soft descriptor limit
/// (`control_connections_cap` in `control.rs`), spelled here because the wire
/// does not say it.
fn connection_cap(soft_limit: u64) -> usize {
    usize::try_from(soft_limit / 4)
        .unwrap_or(usize::MAX)
        .clamp(RPC_LANES, 1_024)
}

/// This process's soft and hard `RLIMIT_NOFILE`.
fn descriptor_limits() -> (u64, u64) {
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: a valid resource id and an exclusively borrowed out-parameter.
    let read = unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &raw mut limit) };
    assert_eq!(read, 0, "read RLIMIT_NOFILE");
    (limit.rlim_cur, limit.rlim_max)
}

/// Give THIS test process room for every connection its tests hold at once
/// (they run in parallel in one process), whatever shell started the suite.
fn roomy_test_process() {
    let (soft, hard) = descriptor_limits();
    let wanted = ROOMY_SOFT_LIMIT.min(hard);
    if soft < wanted {
        let limit = libc::rlimit {
            rlim_cur: wanted,
            rlim_max: hard,
        };
        // SAFETY: a valid resource id and a borrowed, initialized rlimit; raising
        // the soft limit up to the hard one needs no privilege.
        let _ = unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &raw const limit) };
    }
}

/// The scratch world, removed on drop.
struct World(PathBuf);

impl Drop for World {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The instance, killed and reaped on drop, with the lifeline that takes it
/// down if this test process dies first.
struct Reaped(Child, Option<aterm_uds::lifeline::Lifeline>);

impl Drop for Reaped {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
        drop(self.1.take());
    }
}

/// A scratch world of its own per test: the tests of this binary run in
/// parallel, each against its own instance.
fn world(tag: &str) -> Option<World> {
    let name = format!("atln{tag}-{}", std::process::id());
    for base in [std::env::temp_dir(), PathBuf::from("/tmp")] {
        let root = base.join(&name);
        if root.join("run/aterm/lanes.sock.token").as_os_str().len() >= MAX_SOCK_PATH {
            continue;
        }
        if launch_isolation::prepare(&root).is_ok() {
            return Some(World(root));
        }
        let _ = std::fs::remove_dir_all(&root);
    }
    None
}

/// Boot a headless instance on `sock` under a soft descriptor limit of
/// `soft_limit` (the hard limit kept; `None` = SKIP when it cannot have it).
/// Returns the instance and the limit it runs under.
fn boot(root: &Path, sock: &Path, soft_limit: u64) -> Option<(Reaped, u64)> {
    let (_, hard) = descriptor_limits();
    if hard < soft_limit {
        eprintln!(
            "SKIP: the hard descriptor limit {hard} is below the {soft_limit} this test needs"
        );
        return None;
    }
    let log = std::fs::File::create(root.join("instance.log")).ok()?;
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_aterm"));
    launch_isolation::apply(&mut cmd, root);
    cmd.args(["--headless", launch_isolation::NO_REROUTE])
        .arg("--control-sock")
        .arg(sock)
        .stdin(Stdio::null())
        .stdout(log.try_clone().ok()?)
        .stderr(log);
    let limit = libc::rlimit {
        rlim_cur: soft_limit,
        rlim_max: hard,
    };
    // SAFETY: the hook runs in the forked child before `exec`; it only calls
    // `setrlimit` (async-signal-safe, no allocation) on a value copied in, and
    // `last_os_error` reads errno without allocating.
    unsafe {
        cmd.pre_exec(move || {
            if libc::setrlimit(libc::RLIMIT_NOFILE, &raw const limit) == 0 {
                Ok(())
            } else {
                Err(std::io::Error::last_os_error())
            }
        });
    }
    let lifeline = launch_isolation::lifeline(&mut cmd, root);
    let child = match cmd.spawn() {
        Ok(child) => Reaped(child, Some(lifeline)),
        Err(e) => {
            eprintln!("SKIP: cannot launch aterm --headless ({e})");
            return None;
        }
    };
    let deadline = Instant::now() + Duration::from_secs(60);
    while !launch_isolation::control_listening(sock) {
        if Instant::now() > deadline {
            eprintln!("SKIP: the instance never listened on {}", sock.display());
            return None;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Some((child, soft_limit))
}

/// The instance's token, read from beside its socket.
fn token(sock: &Path) -> String {
    let path = format!("{}.token", sock.display());
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read {path}: {e}"))
        .trim()
        .to_string()
}

/// One persistent, authenticated control connection, driven the way
/// `RelayCtl` drives it: a request line, then its one status line. One
/// descriptor each (writes go through the reader's stream), so the tests'
/// own descriptor use stays what the connections cost.
struct Driver {
    reader: BufReader<UnixStream>,
}

impl Driver {
    /// Dial and authenticate; nothing is read yet (the server's reply to a
    /// refused connection is the first line the next request reads).
    fn open(sock: &Path, token: &str) -> Self {
        Self::try_open(sock, token).unwrap_or_else(|e| panic!("dial the control socket: {e}"))
    }

    /// Dial and say nothing: a peer the server gives its handshake time to
    /// (30 s to present a token), on a request lane, while it stays silent.
    fn silent(sock: &Path) -> Self {
        let stream =
            UnixStream::connect(sock).unwrap_or_else(|e| panic!("dial the control socket: {e}"));
        bound_reads(&stream, Duration::from_secs(20));
        Self {
            reader: BufReader::new(stream),
        }
    }

    fn try_open(sock: &Path, token: &str) -> std::io::Result<Self> {
        let stream = UnixStream::connect(sock)?;
        bound_reads(&stream, Duration::from_secs(20));
        let mut driver = Self {
            reader: BufReader::new(stream),
        };
        driver.send(&format!("AUTH {token}"));
        Ok(driver)
    }

    /// Dial, authenticate and have one `version` answered: a persistent
    /// driver, established. A refusal while the request lanes are briefly full
    /// of WORK (a burst of connections being authenticated on a loaded machine)
    /// is the busy line clients retry on — the supervisor rides it as an outage
    /// — so it is retried here; a refusal that LASTS five seconds is the defect
    /// (every lane held by a connection with nothing to ask), reported with the
    /// server's last words.
    fn established(sock: &Path, token: &str) -> Result<Self, String> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let last = match Self::try_open(sock, token) {
                Ok(mut driver) => {
                    let reply = driver.request("version");
                    if reply.starts_with("OK") {
                        return Ok(driver);
                    }
                    reply
                }
                Err(e) => format!("<dial error: {e}>"),
            };
            if Instant::now() > deadline {
                return Err(last);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn stream(&self) -> &UnixStream {
        self.reader.get_ref()
    }

    /// Write one request line. A failed write is not the verdict: a connection
    /// the listener refused may be closed before the line lands (`EPIPE`), and
    /// its busy line is still there for the next read, which decides.
    fn send(&mut self, line: &str) {
        let _ = self.stream().write_all(format!("{line}\n").as_bytes());
    }

    /// A reply ALREADY waiting on the connection, read without blocking: the
    /// listener writes its busy line before it accepts the next peer, so once a
    /// later peer is answered, a refused connection holds that line.
    fn waiting_reply(&mut self) -> Option<String> {
        self.stream()
            .set_nonblocking(true)
            .expect("read without blocking");
        let mut reply = String::new();
        let read = self.reader.read_line(&mut reply);
        self.stream()
            .set_nonblocking(false)
            .expect("blocking reads again");
        match read {
            Ok(0) => Some("<EOF>".to_string()),
            Ok(_) => Some(reply.trim_end().to_string()),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => None,
            Err(e) => Some(format!("<read error: {e}>")),
        }
    }

    fn line(&mut self) -> String {
        let mut reply = String::new();
        match self.reader.read_line(&mut reply) {
            Ok(0) => "<EOF>".to_string(),
            Ok(_) => reply.trim_end().to_string(),
            Err(e) => format!("<read error: {e}>"),
        }
    }

    fn request(&mut self, line: &str) -> String {
        self.send(line);
        self.line()
    }
}

/// Bound every read on `stream`. macOS refuses the option (`EINVAL`) on a
/// socket its peer has already closed — a connection the listener refused and
/// dropped at once — and a read there cannot block anyway: it returns the busy
/// line, then end-of-file.
fn bound_reads(stream: &UnixStream, timeout: Duration) {
    let _ = stream.set_read_timeout(Some(timeout));
}

/// A fresh one-shot client: AUTH and `version` in one write (as `aterm-ctl`
/// sends them), the first reply line — what the server SAID, even to a write
/// that failed — and how long it took.
fn probe(sock: &Path, token: &str) -> (String, Duration) {
    let start = Instant::now();
    let mut stream = match UnixStream::connect(sock) {
        Ok(stream) => stream,
        Err(e) => return (format!("<connect error: {e}>"), start.elapsed()),
    };
    bound_reads(&stream, Duration::from_secs(20));
    // A refused connection may be closed before this lands (`EPIPE`); its busy
    // line is still there to read, and the read decides.
    let _ = stream.write_all(format!("AUTH {token}\nversion\n").as_bytes());
    let mut reply = String::new();
    let reply = match BufReader::new(stream).read_line(&mut reply) {
        Ok(0) => "<EOF>".to_string(),
        Ok(_) => reply.trim_end().to_string(),
        Err(e) => format!("<read error: {e}>"),
    };
    (reply, start.elapsed())
}

/// A fresh idle driver: dialled, authenticated and served one `version` (reads
/// bounded at 5 s), or the reply that refused it and how long that took.
fn idle_driver(sock: &Path, token: &str) -> Result<Driver, (String, Duration)> {
    let start = Instant::now();
    let mut driver = match Driver::try_open(sock, token) {
        Ok(driver) => driver,
        Err(e) => return Err((format!("<dial error: {e}>"), start.elapsed())),
    };
    bound_reads(driver.stream(), Duration::from_secs(5));
    let reply = driver.request("version");
    if reply.starts_with("OK") {
        Ok(driver)
    } else {
        Err((reply, start.elapsed()))
    }
}

/// A momentary refusal, made on purpose: silent peers — dialled, never
/// authenticated — until every request lane is at work on one's handshake, as
/// a burst of new connections leaves them. A peer the listener refused on
/// arrival (a lane was still placing the last driver) holds the busy line once
/// a later client is answered (the listener writes it before it accepts the
/// next peer), and is dialled again. Returns the peers; hanging them up ends
/// the moment.
fn occupy_request_lanes(sock: &Path, token: &str) -> Vec<Driver> {
    let mut silent: Vec<Driver> = Vec::new();
    for _ in 0..40 {
        while silent.len() < RPC_LANES {
            silent.push(Driver::silent(sock));
        }
        let (reply, _) = probe(sock, token);
        silent.retain_mut(|peer| peer.waiting_reply().is_none());
        if silent.len() == RPC_LANES {
            assert_eq!(
                reply, BUSY,
                "a fresh client was served with {RPC_LANES} handshakes in progress"
            );
            return silent;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("the request lanes never all took a silent peer");
}

/// A wait the server can only end at its timeout: `await seq` for a content
/// count this idle instance never reaches.
fn long_wait(timeout_ms: u64) -> String {
    format!("await seq 999999999999 timeout {timeout_ms}")
}

/// One wait the socket ADMITTED, on a fresh connection, and the fresh probe
/// that followed it: `(wait, probe reply, probe time)`. `reopened` counts the
/// waits that were refused on arrival and opened again.
///
/// A request lane that has just answered is still counted busy for a moment
/// after its reply reaches the client, so a wait can arrive while the previous
/// probe's lane is finishing and be refused itself — a moment, not saturation.
/// It is told apart WITHOUT a timer: the listener takes connections in order
/// and writes its busy line before it accepts the next, so once the probe that
/// follows the wait is answered, a refused wait already holds that line. It is
/// read without blocking, and such a wait is opened again (it never held a
/// lane). One connection at a time, never a burst: a burst of fresh
/// connections is work that may fill the request lanes for a moment, which the
/// busy line is FOR.
fn admitted_wait(
    sock: &Path,
    token: &str,
    wait_ms: u64,
    reopened: &mut usize,
) -> (Driver, String, Duration) {
    loop {
        let mut wait = Driver::open(sock, token);
        wait.send(&long_wait(wait_ms));
        std::thread::sleep(Duration::from_millis(25));
        let (reply, took) = probe(sock, token);
        let Some(early) = wait.waiting_reply() else {
            return (wait, reply, took);
        };
        assert_eq!(early, BUSY, "a wait answered before its timeout");
        *reopened += 1;
        assert!(
            *reopened <= 40,
            "waits were refused on arrival {reopened} times"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn idle_and_waiting_drivers_do_not_starve_a_new_client() {
    const IDLE: usize = 12;
    const WAITING: usize = 12;

    roomy_test_process();
    let Some(world) = world("s") else {
        eprintln!("SKIP: no scratch base with a short enough socket path");
        return;
    };
    let sock = world.0.join("run/aterm/lanes.sock");
    // The budget of the window the owner starts from the Dock.
    let Some(_instance) = boot(&world.0, &sock, LAUNCHD_SOFT_LIMIT) else {
        return;
    };
    let token = token(&sock);

    // Twelve drivers, each served once and then left open and idle.
    let mut idle = Vec::new();
    for index in 0..IDLE {
        let driver = Driver::established(&sock, &token).unwrap_or_else(|reply| {
            panic!(
                "idle driver {} of {IDLE} was not served within 5 s: {reply:?}",
                index + 1
            )
        });
        idle.push(driver);
    }
    // Twelve more, each parked in a wait the server ends only at its timeout.
    let mut reopened = 0;
    let mut waiting = Vec::new();
    for index in 0..WAITING {
        let (wait, reply, _) = admitted_wait(&sock, &token, 30_000, &mut reopened);
        assert!(
            reply.starts_with("OK"),
            "a fresh client was refused with {IDLE} idle and {} waiting drivers open: \
             {reply:?}",
            index + 1
        );
        waiting.push(wait);
    }

    // Twenty-four persistent drivers against eight request lanes: every fresh
    // client is still answered, and at once.
    for attempt in 0..20 {
        let (reply, took) = probe(&sock, &token);
        assert!(
            reply.starts_with("OK"),
            "fresh client {attempt} was refused with {IDLE} idle and {WAITING} waiting \
             drivers open: {reply:?}"
        );
        assert!(
            took < Duration::from_secs(5),
            "fresh client {attempt} waited {took:?}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    // The CLI path a person types.
    let mut ctl = Command::new(env!("CARGO_BIN_EXE_aterm"));
    launch_isolation::apply(&mut ctl, &world.0);
    let out = ctl
        .arg("ctl")
        .arg("--sock")
        .arg(&sock)
        .arg("status")
        .stdin(Stdio::null())
        .output()
        .expect("run aterm ctl status");
    assert!(
        out.status.success(),
        "aterm ctl status failed with the drivers open: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    // The persistent-driver contract holds: each idle connection is served
    // again, and two requests written at once are answered in order.
    for (index, driver) in idle.iter_mut().enumerate() {
        let reply = driver.request("version");
        assert!(
            reply.starts_with("OK"),
            "idle driver {index} lost its connection: {reply:?}"
        );
    }
    let first = &mut idle[0];
    first
        .stream()
        .write_all(b"version\nbogus-verb-xyz\n")
        .expect("pipelined pair");
    let (one, two) = (first.line(), first.line());
    assert!(one.starts_with("OK"), "first pipelined reply: {one:?}");
    assert!(
        two.starts_with("ERR"),
        "second pipelined reply answers the second request: {two:?}"
    );
    drop(waiting);
}

#[test]
fn a_pool_saturated_with_waits_still_answers_busy() {
    // Held idle alongside the waits: they must not count toward saturation.
    const IDLE: usize = 16;
    const WAIT_MS: u64 = 15_000;

    roomy_test_process();
    let Some(world) = world("b") else {
        eprintln!("SKIP: no scratch base with a short enough socket path");
        return;
    };
    let sock = world.0.join("run/aterm/lanes.sock");
    // Room for every lane's wait plus the idle drivers: the LANES saturate here,
    // not the connection cap.
    let Some((_instance, soft_limit)) = boot(&world.0, &sock, ROOMY_SOFT_LIMIT) else {
        return;
    };
    assert!(
        connection_cap(soft_limit) > IDLE + RPC_LANES + WAIT_LANES + 8,
        "the connection cap at soft limit {soft_limit} would saturate before the lanes"
    );
    let token = token(&sock);

    let mut idle = Vec::new();
    for _ in 0..IDLE {
        let driver = Driver::established(&sock, &token)
            .unwrap_or_else(|reply| panic!("idle driver served: {reply:?}"));
        idle.push(driver);
    }

    // Open waits one at a time (each one admitted: `admitted_wait`) until a
    // fresh client is refused. A PROBE, too, can arrive while the previous
    // wait's lane is still handing it over, so saturation is judged by a
    // refusal that STAYS: the waits last seconds, a finishing lane milliseconds.
    let started = Instant::now();
    let mut waits: Vec<Driver> = Vec::new();
    let mut reopened = 0;
    let mut refused = None;
    while waits.len() < RPC_LANES + WAIT_LANES + 8 {
        let (wait, mut reply, took) = admitted_wait(&sock, &token, WAIT_MS, &mut reopened);
        waits.push(wait);
        if reply == BUSY {
            // Held for a quarter of a second, or a lane that was finishing.
            for _ in 0..5 {
                std::thread::sleep(Duration::from_millis(50));
                reply = probe(&sock, &token).0;
                if reply != BUSY {
                    break;
                }
            }
        }
        if reply == BUSY {
            refused = Some((waits.len(), took));
            break;
        }
        assert!(
            reply.starts_with("OK"),
            "a fresh client below saturation got {reply:?}"
        );
    }
    let saturated = started.elapsed();
    eprintln!(
        "saturation: {} waits open after {saturated:?}; {reopened} refused on arrival and reopened",
        waits.len()
    );
    let Some((at, took)) = refused else {
        panic!(
            "no fresh client was refused with {} waits open: the busy reply is gone",
            waits.len()
        );
    };
    assert!(
        saturated < Duration::from_millis(WAIT_MS - 3_000),
        "saturating took {saturated:?}, too close to the waits' own end to judge"
    );
    assert!(
        took < Duration::from_secs(1),
        "the busy reply took {took:?}"
    );
    assert_eq!(
        at,
        RPC_LANES + WAIT_LANES,
        "the socket must refuse only once every wait lane and every request lane holds \
         a wait ({IDLE} idle drivers open alongside; {reopened} waits reopened)"
    );

    // The waits end at their timeout; each answers, and the socket recovers.
    for (index, driver) in waits.iter_mut().enumerate() {
        let reply = driver.line();
        assert_eq!(reply, "OK timeout", "wait {index} ended at its timeout");
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let (reply, _) = probe(&sock, &token);
        if reply.starts_with("OK") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the socket never recovered after the waits ended: {reply:?}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    for driver in &mut idle {
        assert!(driver.request("version").starts_with("OK"));
    }
}

#[test]
fn the_connection_past_the_cap_is_told_busy_and_tabs_still_open() {
    // Far past any cap the server may derive from launchd's limit.
    const TRIES: usize = 400;

    roomy_test_process();
    let Some(world) = world("c") else {
        eprintln!("SKIP: no scratch base with a short enough socket path");
        return;
    };
    let sock = world.0.join("run/aterm/lanes.sock");
    let Some((_instance, soft_limit)) = boot(&world.0, &sock, LAUNCHD_SOFT_LIMIT) else {
        return;
    };
    let cap = connection_cap(soft_limit);
    let token = token(&sock);

    // Idle drivers, each served once and kept open, until a refusal STAYS. The
    // busy line is for work too: on a loaded machine every request lane can
    // still be placing an earlier connection when the next one arrives
    // (measured 2026-09-26: one early refusal at 25 open, its retry served
    // 0.4 ms later). A refused connection never counted, so such a moment is
    // dialled again and the filling goes on; the cap is a refusal that holds
    // for half a second — the cap ends only when a connection closes, a lane's
    // moment in milliseconds. So that the retry is never dead code, one such
    // moment is made on purpose at `MOMENT_AT` open (`occupy_request_lanes`)
    // and ends as its silent peers hang up; taking its refusal for the cap
    // would stop the filling there.
    let mut held: Vec<Driver> = Vec::new();
    let mut moment: Option<Vec<Driver>> = None;
    let mut made_moment_retried = false;
    let mut cleared = 0;
    let (refusal, took) = 'fill: loop {
        assert!(
            held.len() < TRIES,
            "{TRIES} connections open at soft limit {soft_limit} and none refused"
        );
        if held.len() == MOMENT_AT && !made_moment_retried && moment.is_none() {
            moment = Some(occupy_request_lanes(&sock, &token));
        }
        let (reply, took) = match idle_driver(&sock, &token) {
            Ok(driver) => {
                held.push(driver);
                continue;
            }
            Err(refused) => refused,
        };
        if reply != BUSY {
            break (reply, took);
        }
        // The moment made on purpose ends here: its silent peers hang up.
        let made = moment.take().is_some();
        let stays = Instant::now() + REFUSAL_STAYS;
        while Instant::now() < stays {
            std::thread::sleep(Duration::from_millis(50));
            match idle_driver(&sock, &token) {
                Ok(driver) => {
                    held.push(driver);
                    if made {
                        made_moment_retried = true;
                    } else {
                        cleared += 1;
                        assert!(
                            cleared <= 40,
                            "{cleared} refusals cleared on retry below the cap: \
                             the lanes are not free of idle connections"
                        );
                    }
                    continue 'fill;
                }
                Err((again, _)) if again == BUSY => {}
                Err(other) => break 'fill other,
            }
        }
        break (reply, took);
    };
    eprintln!(
        "cap: {} open at soft limit {soft_limit}; the moment made at {MOMENT_AT} retried: \
         {made_moment_retried}; {cleared} more momentary refusals retried",
        held.len()
    );
    assert_eq!(
        refusal,
        BUSY,
        "the connection past {} open ones (soft descriptor limit {soft_limit}) must get \
         the busy line, not a process out of descriptors",
        held.len()
    );
    assert_eq!(
        held.len(),
        cap,
        "the socket refuses, and keeps refusing, at a quarter of the soft descriptor \
         limit {soft_limit} ({cleared} momentary refusals retried)"
    );
    assert!(
        made_moment_retried,
        "the refusal made momentary on purpose at {MOMENT_AT} open was never retried"
    );
    assert!(
        took < Duration::from_secs(1),
        "the busy reply took {took:?}"
    );
    for driver in &held {
        bound_reads(driver.stream(), Duration::from_secs(20));
    }

    // The window still has descriptors for what it is for: new tabs.
    for index in 0..3 {
        let reply = held[0].request("spawn");
        assert!(
            reply.starts_with("OK s-"),
            "tab {index} could not open with {cap} control connections held: {reply:?}"
        );
    }
    // And the cap still holds, in the documented words, promptly.
    let (reply, took) = probe(&sock, &token);
    assert_eq!(reply, BUSY, "a fresh client at the cap");
    assert!(
        took < Duration::from_secs(1),
        "the busy reply took {took:?}"
    );
    // Every held connection is still served (persistent drivers keep theirs).
    for (index, driver) in held.iter_mut().enumerate() {
        let reply = driver.request("version");
        assert!(reply.starts_with("OK"), "held driver {index}: {reply:?}");
    }
    // One ends: its place is a fresh client's.
    drop(held.pop());
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let (reply, _) = probe(&sock, &token);
        if reply.starts_with("OK") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "a closed connection never gave its place back: {reply:?}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// `aterm ctl --sock <sock> <args…>` in the world's isolation.
fn aterm_ctl(root: &Path, sock: &Path, args: &[&str]) -> std::process::Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_aterm"));
    launch_isolation::apply(&mut cmd, root);
    cmd.arg("ctl")
        .arg("--sock")
        .arg(sock)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("run aterm ctl")
}

#[test]
fn hung_up_waits_give_their_lanes_back_and_ctl_names_busy() {
    /// Far longer than the test: only a hangup can end these waits in time.
    const WAIT_MS: u64 = 60_000;

    roomy_test_process();
    let Some(world) = world("h") else {
        eprintln!("SKIP: no scratch base with a short enough socket path");
        return;
    };
    let sock = world.0.join("run/aterm/lanes.sock");
    let Some((_instance, soft_limit)) = boot(&world.0, &sock, ROOMY_SOFT_LIMIT) else {
        return;
    };
    assert!(
        connection_cap(soft_limit) > RPC_LANES + WAIT_LANES + 8,
        "the connection cap at soft limit {soft_limit} would saturate before the lanes"
    );
    let token = token(&sock);

    // Saturate the socket with waits (each one admitted), judged by a refusal
    // that STAYS, exactly as the negative control above does.
    let mut waits: Vec<Driver> = Vec::new();
    let mut reopened = 0;
    let saturated = loop {
        assert!(
            waits.len() < RPC_LANES + WAIT_LANES + 8,
            "{} waits open and no fresh client was refused",
            waits.len()
        );
        let (wait, mut reply, _) = admitted_wait(&sock, &token, WAIT_MS, &mut reopened);
        waits.push(wait);
        if reply == BUSY {
            for _ in 0..5 {
                std::thread::sleep(Duration::from_millis(50));
                reply = probe(&sock, &token).0;
                if reply != BUSY {
                    break;
                }
            }
        }
        if reply == BUSY {
            break waits.len();
        }
        assert!(
            reply.starts_with("OK"),
            "a fresh client below saturation got {reply:?}"
        );
    };
    eprintln!("saturated at {saturated} waits ({reopened} refused on arrival and reopened)");

    // The client names a refusal for what it is: an ordinary failure carrying
    // the server's own words, never a transport error.
    let out = aterm_ctl(
        &world.0,
        &sock,
        &["await", "seq", "999999999999", "timeout=50"],
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(1),
        "busy is a plain failure: {stderr}"
    );
    assert!(
        stderr.contains(BUSY),
        "busy must be reported as busy: {stderr}"
    );
    assert!(
        !stderr.contains("Broken pipe") && !stderr.contains("not connected"),
        "busy was misreported: {stderr}"
    );

    // Every waiting client hangs up (a SIGKILLed `aterm ctl` closes exactly
    // so). Their lanes must come back within a hangup poll, not at the minute
    // their waits were given: a fresh client is served, and a fresh wait is
    // admitted and runs to its own (short) timeout.
    let hung_up = Instant::now();
    drop(waits);
    let released = loop {
        let (reply, _) = probe(&sock, &token);
        if reply.starts_with("OK") {
            break hung_up.elapsed();
        }
        assert_eq!(reply, BUSY, "an unexpected answer while the lanes drain");
        assert!(
            hung_up.elapsed() < Duration::from_secs(10),
            "hung-up clients still hold their lanes after {:?}",
            hung_up.elapsed()
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    eprintln!("a lane came back {released:?} after its caller hung up");
    assert!(
        released < Duration::from_millis(1_500),
        "hung-up callers held their lanes for {released:?}"
    );
    let mut fresh = Driver::established(&sock, &token)
        .unwrap_or_else(|reply| panic!("a fresh driver after the hangups: {reply:?}"));
    assert_eq!(fresh.request(&long_wait(20)), "OK timeout");

    // And the listener's health is published.
    let metrics = fresh.request("metrics");
    for field in [
        "control_accepts=",
        "control_last_accept_age_ms=",
        "control_queue_pending=",
        "control_rebinds=",
        "control_busy_replies=",
    ] {
        assert!(metrics.contains(field), "metrics lacks {field}: {metrics}");
    }
}
