// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The window's control socket keeps answering new clients however many
//! PERSISTENT drivers hold connections to it, across a real headless instance.
//!
//! THE DEFECT: a connection held one of eight fixed lanes for as long as it
//! stayed open, request or not, and every supervised agent tab holds one (its
//! loop's `RelayCtl`, parked in a 20 s `await`). Eight supervised tabs refused
//! every other client (`ERR control server busy; retry`).
//!
//! What this drives, over the real socket of the one `aterm` binary:
//!
//! * `idle_and_waiting_drivers_do_not_starve_a_new_client` — twelve idle and
//!   twelve `await`-parked drivers (three times the lanes), then fresh clients,
//!   raw and through `aterm ctl`, are answered every time, and the held
//!   connections still answer in order afterwards.
//! * `waits_past_their_lanes_are_told_busy_and_hung_up_waits_give_their_places_back`,
//!   the NEGATIVE CONTROL: waits fill every wait lane and the request lanes'
//!   wait budget (half of them), fresh clients answered beside each; the next
//!   wait gets the busy line on a connection that stays open, and `aterm ctl`
//!   reports it as busy, never "Broken pipe". Without the budget, waits took
//!   every request lane and refused every fresh client. When the waits hang
//!   up their places come back at once, not at their own timeout (SIGKILLed
//!   `aterm ctl await … timeout=20000` clients once held every lane for 20 s).
//!   The listener's health fields are on `metrics`.
//! * `pipelined_waits_are_held_while_wait_lanes_are_free`: a wait with a
//!   request written right behind it (`RelayClient::dial_via_local`'s `dial
//!   <name>` then `version`) moves to a wait lane carrying that request, so
//!   twice the request lanes' worth are all held, none told busy, and each
//!   `version` is answered after its wait.
//! * `the_connection_past_the_cap_is_told_busy_and_tabs_still_open`: the
//!   open-connection cap is a quarter of the descriptor limit. Under launchd's
//!   default soft `RLIMIT_NOFILE` of 256 (a window started from the Dock), idle
//!   drivers are opened until a refusal STAYS: it must be the busy line, at
//!   exactly the cap, and the window must still open tabs. A fixed 1024 let
//!   about 240 idle connections exhaust the descriptors, so clients got a
//!   dropped connection and new tabs failed. `aterm ctl` at the cap prints the
//!   busy line both for a sent verb (`status`) and for its own discovery (`ls`).
//! * `peers_that_stop_mid_exchange_take_no_lane`: twice the request lanes'
//!   worth each of peers that dialled and said nothing, stopped half way
//!   through a request line, stopped half way through a frame's body, or did
//!   not read a large answer (`help --full`). Each once held a request lane
//!   while the server waited on it (a deadline on the wait only made it
//!   reconnect); now the parker holds them, fresh clients are answered beside
//!   them, and each is served once it goes on, its answer whole.
//!
//! Every instance runs under a descriptor limit its test names, so no verdict
//! depends on the shell the suite was started from.
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

/// The server's request lanes (`CONTROL_WORKERS` in `aterm-gui/src/control.rs`),
/// its wait lanes (`CONTROL_WAIT_WORKERS`) and the request lanes that may serve
/// a wait at once (`CONTROL_INLINE_WAITS`): where the negative control expects
/// a wait to be told busy, spelled here because the wire does not say it.
const RPC_LANES: usize = 8;
const WAIT_LANES: usize = 64;
const INLINE_WAITS: usize = RPC_LANES / 2;

/// The busy reply, word for word (`aterm_types::control_verbs::CONTROL_BUSY_REPLY`:
/// the listener's to a connection it cannot admit, a lane's to a wait none of
/// its lanes may take).
const BUSY: &str = "ERR control server busy; retry";

/// How long a refusal must last to be read as the socket's verdict rather than
/// a moment: a lane places a connection in milliseconds on an idle machine
/// and can take a scheduler's seconds on a loaded one, while a cap or a
/// saturation lasts until a connection closes or a wait ends. Paid only once a
/// refusal is seen (a probe every 50 ms until it clears).
const REFUSAL_STAYS: Duration = Duration::from_secs(5);

/// How long a wait for something that MUST happen may take before it is read
/// as a hang (AGENTS.md: a hang detector is a minute, never a latency budget).
const HANG: Duration = Duration::from_secs(60);

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
    /// (30 s from admission to present a token), in the parker, not on a lane.
    fn silent(sock: &Path) -> Self {
        let stream =
            UnixStream::connect(sock).unwrap_or_else(|e| panic!("dial the control socket: {e}"));
        bound_reads(&stream, HANG);
        Self {
            reader: BufReader::new(stream),
        }
    }

    fn try_open(sock: &Path, token: &str) -> std::io::Result<Self> {
        let stream = UnixStream::connect(sock)?;
        bound_reads(&stream, HANG);
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
    /// — so it is retried here; a refusal that LASTS (every lane held by a
    /// connection with nothing to ask — the defect — lasts for ever) fails at
    /// the hang detector, reported with the server's last words.
    fn established(sock: &Path, token: &str) -> Result<Self, String> {
        let deadline = Instant::now() + HANG;
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
        self.send_raw(format!("{line}\n").as_bytes());
    }

    /// Write `bytes` as they are: part of a request, its newline still to come.
    fn send_raw(&mut self, bytes: &[u8]) {
        let _ = self.stream().write_all(bytes);
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
/// that failed. Its reads are bounded at [`HANG`]: a client queued rather than
/// answered reads an error, not a late `OK` (the waits a test queues it behind
/// outlast that bound).
fn probe(sock: &Path, token: &str) -> String {
    let mut stream = match UnixStream::connect(sock) {
        Ok(stream) => stream,
        Err(e) => return format!("<connect error: {e}>"),
    };
    bound_reads(&stream, HANG);
    // A refused connection may be closed before this lands (`EPIPE`); its busy
    // line is still there to read, and the read decides.
    let _ = stream.write_all(format!("AUTH {token}\nversion\n").as_bytes());
    let mut reply = String::new();
    match BufReader::new(stream).read_line(&mut reply) {
        Ok(0) => "<EOF>".to_string(),
        Ok(_) => reply.trim_end().to_string(),
        Err(e) => format!("<read error: {e}>"),
    }
}

/// A fresh idle driver: dialled, authenticated and served one `version` (reads
/// bounded at [`HANG`], as a [`probe`]'s are: a refused one reads its busy line
/// at once), or the reply that refused it.
fn idle_driver(sock: &Path, token: &str) -> Result<Driver, String> {
    let mut driver = Driver::try_open(sock, token).map_err(|e| format!("<dial error: {e}>"))?;
    bound_reads(driver.stream(), HANG);
    let reply = driver.request("version");
    if reply.starts_with("OK") {
        Ok(driver)
    } else {
        Err(reply)
    }
}

/// Text no row of this idle instance holds, until a test types it into the
/// session.
const WAIT_TEXT: &str = "zq-no-row-holds-this-qz";

/// A wait the server can only end at its timeout (or when a test types
/// [`WAIT_TEXT`]): `await match` for text no row of this idle instance holds.
/// (Not `await seq <huge>`: an anchor above the current count is read as one
/// from another grid or process and latched at once.)
fn long_wait(timeout_ms: u64) -> String {
    format!("await match {WAIT_TEXT} timeout {timeout_ms}")
}

/// One wait the socket ADMITTED, on a fresh connection, and the fresh probe
/// that followed it: `(wait, probe reply)`. `reopened` counts the waits that
/// were refused on arrival and opened again.
///
/// A request lane that has just answered is still counted busy for a moment
/// after its reply reaches the client, so a wait can arrive while the previous
/// probe's lane is finishing and be refused itself — a moment, not saturation.
/// It is told apart WITHOUT a timer: the listener takes connections in order
/// and writes its busy line before it accepts the next, so once the probe that
/// follows the wait is answered, a refused wait already holds that line. It is
/// read without blocking, and such a wait is opened again (it never held a
/// lane): the listener dropped it, so its end-of-file follows the line. A wait
/// a LANE told busy (no lane may take it) keeps its connection open and has
/// nothing after the line — that is no moment, and fails here. One connection
/// at a time, never a burst: a burst of fresh connections is work that may
/// fill the request lanes for a moment, which the busy line is FOR.
fn admitted_wait(sock: &Path, token: &str, wait_ms: u64, reopened: &mut usize) -> (Driver, String) {
    loop {
        let mut wait = Driver::open(sock, token);
        wait.send(&long_wait(wait_ms));
        std::thread::sleep(Duration::from_millis(25));
        let reply = probe(sock, token);
        let Some(early) = wait.waiting_reply() else {
            return (wait, reply);
        };
        assert_eq!(early, BUSY, "a wait answered before its timeout");
        let after = wait.waiting_reply();
        assert_eq!(
            after.as_deref(),
            Some("<EOF>"),
            "a wait was told busy on a connection the socket kept — no lane may take it"
        );
        *reopened += 1;
        assert!(
            *reopened <= 40,
            "waits were refused on arrival {reopened} times"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Whether `reply` — a probe's answer — is a refusal that STAYS: [`BUSY`], and
/// still busy on every probe for [`REFUSAL_STAYS`]. A lane still handing over
/// the previous wait answers a later probe; a saturated socket refuses until a
/// wait ends, one at its cap until a connection closes.
///
/// `Err` is the socket's answer when the refusal did not stay: the probe that
/// ended it (a momentary refusal), or `reply` itself when it was no refusal at
/// all. The caller judges that answer.
fn refusal_stays(sock: &Path, token: &str, reply: &str) -> Result<(), String> {
    if reply != BUSY {
        return Err(reply.to_string());
    }
    let stays = Instant::now() + REFUSAL_STAYS;
    while Instant::now() < stays {
        std::thread::sleep(Duration::from_millis(50));
        let again = probe(sock, token);
        if again != BUSY {
            return Err(again);
        }
    }
    Ok(())
}

/// A fresh client's answer, judged as a verdict: a probe that is not [`BUSY`]
/// is its answer; a busy one is probed again until it clears, and stays
/// [`BUSY`] only if the refusal STAYS ([`refusal_stays`]) — a lane still
/// placing the last connection refuses for a moment, a socket out of request
/// lanes until something ends.
fn answered(sock: &Path, token: &str) -> String {
    let reply = probe(sock, token);
    match refusal_stays(sock, token, &reply) {
        Ok(()) => reply,
        Err(answer) => answer,
    }
}

#[test]
fn idle_and_waiting_drivers_do_not_starve_a_new_client() {
    const IDLE: usize = 12;
    const WAITING: usize = 12;
    /// The waits' own timeout: a fresh client queued behind them would be
    /// answered only when they end, far past a probe's [`HANG`] read bound.
    const WAIT_MS: u64 = 180_000;

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
                "idle driver {} of {IDLE} was not served within {HANG:?}: {reply:?}",
                index + 1
            )
        });
        idle.push(driver);
    }
    // Twelve more, each parked in a wait the server ends only at its timeout.
    let mut reopened = 0;
    let mut waiting = Vec::new();
    for index in 0..WAITING {
        let (wait, reply) = admitted_wait(&sock, &token, WAIT_MS, &mut reopened);
        assert!(
            reply.starts_with("OK"),
            "a fresh client was refused with {IDLE} idle and {} waiting drivers open: \
             {reply:?}",
            index + 1
        );
        waiting.push(wait);
    }

    // Twenty-four persistent drivers against eight request lanes: every fresh
    // client is still answered.
    for attempt in 0..20 {
        let reply = probe(&sock, &token);
        assert!(
            reply.starts_with("OK"),
            "fresh client {attempt} was refused with {IDLE} idle and {WAITING} waiting \
             drivers open: {reply:?}"
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
    // still be placing an earlier connection when the next one arrives (one
    // early refusal at 25 open was measured, its retry served 0.4 ms later).
    // Such a moment is dialled again and the filling goes on; the cap is a
    // refusal that holds for [`REFUSAL_STAYS`]. No client can make a moment on
    // purpose any more: silent peers made one here until a peer's silence
    // stopped holding a lane (`peers_that_stop_mid_exchange_take_no_lane`).
    let mut held: Vec<Driver> = Vec::new();
    let mut cleared = 0;
    let refusal = 'fill: loop {
        assert!(
            held.len() < TRIES,
            "{TRIES} connections open at soft limit {soft_limit} and none refused"
        );
        let reply = match idle_driver(&sock, &token) {
            Ok(driver) => {
                held.push(driver);
                continue;
            }
            Err(refused) => refused,
        };
        if reply != BUSY {
            break reply;
        }
        let stays = Instant::now() + REFUSAL_STAYS;
        while Instant::now() < stays {
            std::thread::sleep(Duration::from_millis(50));
            match idle_driver(&sock, &token) {
                Ok(driver) => {
                    held.push(driver);
                    cleared += 1;
                    assert!(
                        cleared <= 40,
                        "{cleared} refusals cleared on retry below the cap: \
                         the lanes are not free of idle connections"
                    );
                    continue 'fill;
                }
                Err(again) if again == BUSY => {}
                Err(other) => break 'fill other,
            }
        }
        break reply;
    };
    eprintln!(
        "cap: {} open at soft limit {soft_limit}; {cleared} momentary refusals retried",
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
    for driver in &held {
        bound_reads(driver.stream(), HANG);
    }

    // The window still has descriptors for what it is for: new tabs.
    for index in 0..3 {
        let reply = held[0].request("spawn");
        assert!(
            reply.starts_with("OK s-"),
            "tab {index} could not open with {cap} control connections held: {reply:?}"
        );
    }
    // And the cap still holds, in the documented words.
    let reply = probe(&sock, &token);
    refusal_stays(&sock, &token, &reply)
        .unwrap_or_else(|reply| panic!("a fresh client at the cap got {reply:?}"));
    // `aterm ctl` at the cap reports the server's own busy line, for a verb it
    // sends (`status`) and for the discovery it answers itself (`ls`) — never
    // the write's `Broken pipe`, never "did not answer".
    for verb in ["status", "ls"] {
        let out = aterm_ctl(&world.0, &sock, &[verb]);
        let said = String::from_utf8_lossy(&out.stderr);
        assert!(
            !out.status.success(),
            "aterm ctl {verb} succeeded at the cap: {said}"
        );
        assert!(
            said.contains(BUSY),
            "aterm ctl {verb} at the cap must say {BUSY:?}; it said: {said}"
        );
        for wrong in ["did not answer", "Broken pipe"] {
            assert!(
                !said.contains(wrong),
                "aterm ctl {verb} at the cap said {wrong:?}: {said}"
            );
        }
    }
    // Every held connection is still served (persistent drivers keep theirs).
    for (index, driver) in held.iter_mut().enumerate() {
        let reply = driver.request("version");
        assert!(reply.starts_with("OK"), "held driver {index}: {reply:?}");
    }
    // One ends: its place is a fresh client's.
    drop(held.pop());
    let deadline = Instant::now() + HANG;
    loop {
        let reply = probe(&sock, &token);
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
fn waits_past_their_lanes_are_told_busy_and_hung_up_waits_give_their_places_back() {
    // Held idle alongside the waits: they must not count.
    const IDLE: usize = 16;
    // Waits past every lane a wait may take.
    const PAST: usize = 4;
    /// Far longer than the test: only a hangup can end these waits in time. A
    /// wait that ended on its own would give its place back with no hangup and
    /// pass the drain below vacuously, so this outlives a loaded gate's
    /// filling, its [`REFUSAL_STAYS`] and the drain's [`HANG`] by far, and the
    /// test checks that it did, below.
    const WAIT_MS: u64 = 180_000;

    roomy_test_process();
    let Some(world) = world("h") else {
        eprintln!("SKIP: no scratch base with a short enough socket path");
        return;
    };
    let sock = world.0.join("run/aterm/lanes.sock");
    // Room for every wait plus the idle drivers: the LANES are what is judged
    // here, not the connection cap.
    let Some((_instance, soft_limit)) = boot(&world.0, &sock, ROOMY_SOFT_LIMIT) else {
        return;
    };
    assert!(
        connection_cap(soft_limit) > IDLE + WAIT_LANES + INLINE_WAITS + PAST + 8,
        "the connection cap at soft limit {soft_limit} would be reached before the lanes"
    );
    let token = token(&sock);

    let mut idle = Vec::new();
    for _ in 0..IDLE {
        let driver = Driver::established(&sock, &token)
            .unwrap_or_else(|reply| panic!("idle driver served: {reply:?}"));
        idle.push(driver);
    }

    // Every wait lane, then the request lanes' wait budget: each wait admitted
    // (`admitted_wait`) and held, and a fresh client answered beside it.
    let first_opened = Instant::now();
    let mut reopened = 0;
    let mut waits: Vec<Driver> = Vec::new();
    while waits.len() < WAIT_LANES + INLINE_WAITS {
        let (wait, reply) = admitted_wait(&sock, &token, WAIT_MS, &mut reopened);
        waits.push(wait);
        let reply = if reply == BUSY {
            answered(&sock, &token)
        } else {
            reply
        };
        assert!(
            reply.starts_with("OK"),
            "a fresh client was refused with {} waits open: {reply:?}",
            waits.len()
        );
    }

    // Past them: each further wait is told busy, on its own connection, which
    // stays open and is served; and fresh clients still are.
    let mut told: Vec<Driver> = Vec::new();
    let mut refused_on_arrival = 0;
    while told.len() < PAST {
        let mut wait = Driver::open(&sock, &token);
        let reply = wait.request(&long_wait(WAIT_MS));
        assert_eq!(
            reply,
            BUSY,
            "wait {} past every wait lane and the request lanes' budget ({} waits held, \
             {IDLE} idle drivers)",
            told.len() + 1,
            waits.len()
        );
        if !wait.request("version").starts_with("OK") {
            // Refused on arrival, not told: a lane was still placing the last
            // connection, and this one was dropped. Dialled again.
            refused_on_arrival += 1;
            assert!(
                refused_on_arrival <= 40,
                "waits were refused on arrival {refused_on_arrival} times"
            );
            std::thread::sleep(Duration::from_millis(50));
            continue;
        }
        let reply = answered(&sock, &token);
        assert!(
            reply.starts_with("OK"),
            "a fresh client was refused beside every wait: {reply:?}"
        );
        told.push(wait);
    }
    eprintln!(
        "{} waits held and {PAST} told busy after {:?}; {reopened} + {refused_on_arrival} \
         refused on arrival and reopened",
        waits.len(),
        first_opened.elapsed()
    );

    // The CLI path a person types, beside every wait: a request is answered,
    // and a wait no lane may take is refused as busy — an ordinary failure
    // carrying the server's own words, never a transport error.
    let out = aterm_ctl(&world.0, &sock, &["status"]);
    assert!(
        out.status.success(),
        "aterm ctl status failed beside every wait: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let out = aterm_ctl(
        &world.0,
        &sock,
        &["await", "match", "zq-no-row-holds-this-qz", "timeout=50"],
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
    // Idle drivers are served beside every wait, and so are the connections
    // whose waits were told busy.
    for driver in idle.iter_mut().chain(told.iter_mut()) {
        assert!(driver.request("version").starts_with("OK"));
    }

    // Every waiting client hangs up (a SIGKILLed `aterm ctl` closes exactly
    // so). Their places must come back at the hangup, not at the end of the
    // minutes their waits were given: a fresh wait is held and runs to its own
    // (short) timeout. Every wait must still be open through the whole drain:
    // one that could end on its own inside it would free its place with no
    // hangup (the oldest was opened after `first_opened`, so this is
    // conservative).
    let hung_up = Instant::now();
    let open_for = hung_up.duration_since(first_opened);
    assert!(
        open_for + HANG < Duration::from_millis(WAIT_MS),
        "the first wait was {open_for:?} old at the hangup: it could end on its own \
         before the drain is judged, so the check would be vacuous"
    );
    drop(waits);
    let mut fresh = loop {
        let mut fresh = Driver::established(&sock, &token)
            .unwrap_or_else(|reply| panic!("a fresh driver after the hangups: {reply:?}"));
        let reply = fresh.request(&long_wait(20));
        if reply == "OK timeout" {
            break fresh;
        }
        assert_eq!(reply, BUSY, "an unexpected answer while the places drain");
        assert!(
            hung_up.elapsed() < HANG,
            "hung-up clients still hold their places after {:?}",
            hung_up.elapsed()
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    eprintln!(
        "a place came back {:?} after its caller hung up",
        hung_up.elapsed()
    );
    for driver in &mut idle {
        assert!(driver.request("version").starts_with("OK"));
    }

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

/// A wait with `version` written right behind it, on a connection of its own,
/// in one write or — `relay_shape` — the way `RelayClient::write_line` writes
/// (each line, then its newline: six writes). `Ok` when the socket holds it;
/// `Err` with what it said when a lane told it busy and then served the
/// `version` behind it (the wait was turned away). A connection refused on
/// ARRIVAL — the busy line, then end-of-file, no lane ever held it (a moment
/// while a lane places the last connection) — is dialled again, counted in
/// `reopened`. The probe after it orders the check: the listener writes its
/// busy line before it accepts the next peer.
fn pipelined_wait(
    sock: &Path,
    token: &str,
    wait_ms: u64,
    relay_shape: bool,
    reopened: &mut usize,
) -> Result<Driver, String> {
    loop {
        let stream =
            UnixStream::connect(sock).unwrap_or_else(|e| panic!("dial the control socket: {e}"));
        bound_reads(&stream, HANG);
        let mut driver = Driver {
            reader: BufReader::new(stream),
        };
        let lines = [
            format!("AUTH {token}"),
            long_wait(wait_ms),
            "version".to_string(),
        ];
        if relay_shape {
            for line in &lines {
                let _ = driver.stream().write_all(line.as_bytes());
                let _ = driver.stream().write_all(b"\n");
            }
        } else {
            let _ = driver
                .stream()
                .write_all(format!("{}\n", lines.join("\n")).as_bytes());
        }
        std::thread::sleep(Duration::from_millis(25));
        let _ = probe(sock, token);
        let Some(early) = driver.waiting_reply() else {
            return Ok(driver);
        };
        assert_eq!(early, BUSY, "a pipelined wait answered before its timeout");
        let next = driver.line();
        if next != "<EOF>" {
            return Err(format!("{early:?}, then {next:?}"));
        }
        *reopened += 1;
        assert!(
            *reopened <= 40,
            "pipelined waits were refused on arrival {reopened} times"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// One wait that ends on its OWN soon, with `version` written right behind it
/// in the same write: the two lines the connection answers, `(wait, version)`.
/// The carry must hold for a wait that is answered by its timeout as for one
/// that is answered by an event.
///
/// Nothing here is judged by a clock: the wait may end before this reads
/// anything (a loaded machine), and its answer is the same line. A connection
/// refused on ARRIVAL (the busy line, then end-of-file: no lane ever held it, a
/// moment while a lane places the last connection) is dialled again; a wait a
/// lane told busy KEEPS its connection and serves the `version` behind it, and
/// is reported as such.
fn carried_timeout(sock: &Path, token: &str, text: &str) -> Result<(String, String), String> {
    for _ in 0..40 {
        let stream =
            UnixStream::connect(sock).unwrap_or_else(|e| panic!("dial the control socket: {e}"));
        bound_reads(&stream, HANG);
        let mut driver = Driver {
            reader: BufReader::new(stream),
        };
        let lines = [
            format!("AUTH {token}"),
            format!("await match {text} timeout 300"),
            "version".to_string(),
        ];
        let _ = driver
            .stream()
            .write_all(format!("{}\n", lines.join("\n")).as_bytes());
        let wait = driver.line();
        if wait != BUSY {
            return Ok((wait, driver.line()));
        }
        let next = driver.line();
        if next != "<EOF>" {
            return Err(format!("{wait:?}, then {next:?}"));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err("refused on arrival 40 times".to_string())
}

#[test]
fn pipelined_waits_are_held_while_wait_lanes_are_free() {
    // Twice the request lanes, so four times their wait budget.
    const PIPELINED: usize = 2 * RPC_LANES;
    /// The waits' own timeout: a hang bound they never reach. They end by the
    /// event they wait for (the text typed below), so the test takes as long as
    /// the server takes, not as long as a wait's clock.
    const WAIT_MS: u64 = 180_000;

    roomy_test_process();
    let Some(world) = world("p") else {
        eprintln!("SKIP: no scratch base with a short enough socket path");
        return;
    };
    let sock = world.0.join("run/aterm/lanes.sock");
    let Some((_instance, soft_limit)) = boot(&world.0, &sock, LAUNCHD_SOFT_LIMIT) else {
        return;
    };
    assert!(
        connection_cap(soft_limit) > PIPELINED + 8,
        "the connection cap at soft limit {soft_limit} would be reached first"
    );
    let token = token(&sock);

    let started = Instant::now();
    let mut reopened = 0;
    let mut held = Vec::new();
    for index in 0..PIPELINED {
        let relay_shape = index % 2 == 1;
        match pipelined_wait(&sock, &token, WAIT_MS, relay_shape, &mut reopened) {
            Ok(driver) => held.push(driver),
            Err(said) => panic!(
                "pipelined wait {} of {PIPELINED} ({}) was told busy with {} held and the \
                 wait lanes free: {said}",
                index + 1,
                if relay_shape {
                    "six writes"
                } else {
                    "one write"
                },
                held.len()
            ),
        }
    }
    eprintln!(
        "{PIPELINED} pipelined waits held after {:?}; {reopened} refused on arrival",
        started.elapsed()
    );
    // Not a latency budget: a wait that had already ended would have been
    // judged "held" vacuously. Nothing has happened to any of them yet, so
    // none may have an answer waiting (their own end is minutes away).
    for (index, driver) in held.iter_mut().enumerate() {
        assert_eq!(
            driver.waiting_reply(),
            None,
            "pipelined wait {index} ended before anything happened to it: the check was vacuous"
        );
    }
    let reply = answered(&sock, &token);
    assert!(
        reply.starts_with("OK"),
        "a fresh client was refused beside {PIPELINED} pipelined waits: {reply:?}"
    );
    // A wait that ends by its own timeout, carried like the rest, answers and
    // then serves the `version` behind it (beside the waits still held).
    let (wait, version) = carried_timeout(&sock, &token, "zq-nothing-types-this-qz")
        .unwrap_or_else(|said| {
            panic!("a pipelined wait with a short timeout was told busy: {said}")
        });
    assert_eq!(
        wait, "OK timeout",
        "the short pipelined wait ended at its timeout"
    );
    assert!(
        version.starts_with("OK") && version != "OK timeout",
        "the short pipelined wait: then its `version`, got {version:?}"
    );
    // Each held wait ends by its event: the text a person would type shows on
    // one visible row of the session (its tty echoes it; no newline, so nothing
    // runs), and every `await match` on it latches. Then the request behind
    // each wait is answered: in order, on its own connection.
    let mut typist = Driver::established(&sock, &token)
        .unwrap_or_else(|reply| panic!("a driver to type with: {reply:?}"));
    let sent = typist.request(&format!("send {WAIT_TEXT}"));
    assert!(
        sent.starts_with("OK"),
        "typing {WAIT_TEXT:?} was answered {sent:?}"
    );
    for (index, driver) in held.iter_mut().enumerate() {
        let wait = driver.line();
        assert!(
            wait.starts_with("OK match "),
            "pipelined wait {index} held until the text showed: {wait:?}"
        );
        let next = driver.line();
        assert!(
            next.starts_with("OK") && !next.starts_with("OK match "),
            "pipelined wait {index}: then its `version`, got {next:?}"
        );
    }
}

/// Read until a line that starts with `upto` has arrived: the count of bytes
/// before that line, and the line itself.
fn read_through(reader: &mut BufReader<UnixStream>, upto: &str) -> (usize, String) {
    let mut before = 0;
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => panic!("the connection ended {before} bytes in, before {upto:?}"),
            Ok(_) if line.starts_with(upto) => return (before, line.trim_end().to_string()),
            Ok(read) => before += read,
            Err(e) => panic!("read {before} bytes, then: {e}"),
        }
    }
}

#[test]
fn peers_that_stop_mid_exchange_take_no_lane() {
    /// Of each kind: twice the request lanes, each of which it once held.
    const EACH: usize = 2 * RPC_LANES;

    roomy_test_process();
    let Some(world) = world("m") else {
        eprintln!("SKIP: no scratch base with a short enough socket path");
        return;
    };
    let sock = world.0.join("run/aterm/lanes.sock");
    let Some(_instance) = boot(&world.0, &sock, ROOMY_SOFT_LIMIT) else {
        return;
    };
    let token = token(&sock);
    // What a `help --full` answer is, read whole by a client that reads.
    let whole = {
        let mut reader = Driver::open(&sock, &token);
        reader.send("help --full");
        reader.send("version");
        read_through(&mut reader.reader, "OK version=").0
    };

    // Each peer is placed one at a time, never in a burst (a burst of fresh
    // connections is work that may fill the request lanes for a moment, which
    // the busy line is FOR). Dialled, and not a byte said: one the listener
    // refused on arrival holds the busy line once a later client is answered,
    // and is dialled again.
    let mut silent: Vec<Driver> = Vec::new();
    while silent.len() < EACH {
        let mut peer = Driver::silent(&sock);
        answered(&sock, &token);
        if peer.waiting_reply().is_none() {
            silent.push(peer);
        }
    }
    let established = |what: &str| {
        Driver::established(&sock, &token)
            .unwrap_or_else(|reply| panic!("a {what} peer was not served: {reply:?}"))
    };
    // Half a request line.
    let mut halfway: Vec<Driver> = (0..EACH)
        .map(|_| {
            let mut driver = established("half-sent");
            driver.send_raw(b"vers");
            driver
        })
        .collect();
    // A frame's header and half its body (into the idle shell's input line).
    let mut framing: Vec<Driver> = (0..EACH)
        .map(|_| {
            let mut driver = established("half-framed");
            driver.send_raw(b"feed-bin 6\nabc");
            driver
        })
        .collect();
    // A large answer asked for, and not read: then the request behind it.
    let mut unread: Vec<Driver> = (0..EACH)
        .map(|_| {
            let mut driver = established("unread");
            driver.send("help --full");
            driver.send("version");
            driver
        })
        .collect();

    // Sixty-four peers the server is waiting on, against eight request lanes:
    // every fresh client is answered (a moment of work is waited out), raw and
    // through `aterm ctl`.
    for attempt in 0..20 {
        let reply = answered(&sock, &token);
        assert!(
            reply.starts_with("OK"),
            "fresh client {attempt} was refused beside {EACH} silent, {EACH} half-sent, \
             {EACH} half-framed and {EACH} unread peers: {reply:?}"
        );
    }
    let out = aterm_ctl(&world.0, &sock, &["version"]);
    assert!(
        out.status.success(),
        "aterm ctl version beside them: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Each goes on, and is served: nothing it began was lost.
    for (index, driver) in halfway.iter_mut().enumerate() {
        driver.send_raw(b"ion\n");
        let reply = driver.line();
        assert!(
            reply.starts_with("OK version="),
            "half-sent peer {index}: {reply:?}"
        );
    }
    for (index, driver) in framing.iter_mut().enumerate() {
        driver.send_raw(b"def");
        assert_eq!(driver.line(), "OK 6 bytes", "half-framed peer {index}");
    }
    for (index, driver) in unread.iter_mut().enumerate() {
        let (got, _) = read_through(&mut driver.reader, "OK version=");
        assert_eq!(
            got, whole,
            "unread peer {index}: its answer, whole, then the next"
        );
    }
    drop(silent);
}
