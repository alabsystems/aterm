// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The window's control socket keeps answering new clients however many persistent drivers
//! hold connections to it, over the real socket of a headless instance of the one `aterm`
//! binary. Every supervised agent tab holds a connection (its loop's `RelayCtl`, parked in a
//! 20 s `await`), so a held connection must not hold a lane.
//!
//! * `idle_and_waiting_drivers_do_not_starve_a_new_client`: twelve idle and twelve
//!   `await`-parked drivers, then fresh clients, raw and through `aterm ctl`, are answered
//!   every time, and the held connections still answer in order.
//! * `waits_past_their_lanes_are_told_busy_and_hung_up_waits_give_their_places_back`, the
//!   negative control: waits fill every wait lane and the request lanes' wait budget; the
//!   next wait gets the busy line on a connection that stays open (`aterm ctl`
//!   reports busy, never "Broken pipe"), and waits that hang up give their places back at
//!   once. The listener's health fields are on `metrics`.
//! * `pipelined_waits_are_held_while_wait_lanes_are_free`: a wait with a request right behind
//!   it (a wait then `version`) moves to a wait lane carrying that request, so twice the
//!   request lanes' worth are held, none told busy.
//! * `the_connection_past_the_cap_takes_the_stalest_place_and_tabs_still_open`: the
//!   open-connection cap is a quarter of the descriptor limit. Under launchd's default soft
//!   `RLIMIT_NOFILE` of 256, with idle drivers holding the cap, the connection past it is
//!   served in the place of the driver served longest ago, which reads the hang-up, while the
//!   window still opens tabs; `aterm ctl` at the cap is answered, for a sent verb and for its
//!   own discovery, and the drivers it displaced are the stalest. Until 2026-10-19 the
//!   connection past the cap got the busy line for as long as the drivers stayed
//!   (ATERM-KI-1015-13). At a cap of waits, where nothing gives way, the connection past it
//!   gets the busy line, and `aterm ctl` prints it for a sent verb and for its discovery.
//! * `peers_that_stop_mid_exchange_take_no_lane`: peers that dialled and said nothing,
//!   stopped half way through a request line or a frame's body, or did not read a large
//!   answer are held by the parker, not a request lane; fresh clients are answered beside
//!   them, and each is served whole once it goes on.
//!
//! Every instance runs under a descriptor limit its test names, so no verdict
//! depends on the shell the suite was started from. And no verdict depends on
//! how long a refusal lasted: the lanes and the cap are known numbers, so a
//! busy line below them is a moment, asked again until it clears (a [`HANG`]
//! detector), and one at them is the cap.
//!
//! ISOLATION: scratch HOME/XDG roots, a private explicit control socket, a
//! config with every automatic lane off (the harness included), `--no-reroute`,
//! `SHELL=/bin/sh` (`support/launch_isolation.rs`). An instance that cannot
//! start FAILS the test; an environment refusal (no short scratch path, a hard
//! descriptor limit below the test's, a log or spawn refusal) is COULD NOT RUN.
//! Readiness is the instance's own word (`--ready-fd`, `aterm_agent::ready`).

#![cfg(unix)]
#![allow(
    clippy::disallowed_types,
    reason = "test code: a stand-in child may inherit descriptors"
)]

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use aterm_spec::host::could_not_run;

#[path = "support/ctl_accept.rs"]
mod ctl_accept;
#[path = "support/launch_isolation.rs"]
mod launch_isolation;

/// The server's request lanes (`CONTROL_WORKERS` in `aterm-gui/src/control.rs`),
/// its wait lanes (`CONTROL_WAIT_WORKERS`) and the request lanes that may serve
/// a wait at once (`CONTROL_INLINE_WAITS`: what the artifact captures,
/// `ARTIFACT_HANDOFF_LIMIT` of them, leave of the request lanes, less one):
/// where the negative control expects a wait to be told busy, spelled here
/// because the wire does not say it.
const RPC_LANES: usize = 8;
const WAIT_LANES: usize = 64;
const ARTIFACT_CAPTURES: usize = 4;
const INLINE_WAITS: usize = RPC_LANES - ARTIFACT_CAPTURES - 1;

/// The busy reply, word for word (`aterm_types::control_verbs::CONTROL_BUSY_REPLY`:
/// the listener's to a connection it cannot admit, a lane's to a wait none of
/// its lanes may take).
const BUSY: &str = "ERR control server busy; retry";

/// How long a wait for something that must happen may take before it is read as a hang
/// (AGENTS.md).
const HANG: Duration = Duration::from_secs(60);

/// The soft descriptor limit launchd gives an app it starts (the Dock, Finder,
/// `open`): the window's own budget, which aterm does not raise.
const LAUNCHD_SOFT_LIMIT: u64 = 256;

/// A limit roomy enough that the connection cap (a quarter of it, at most 1024) is never what a
/// test about the lanes runs into.
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

/// Give this test process room for every connection its tests hold at once (they run in
/// parallel in one process), whatever shell started the suite.
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
fn world(tag: &str) -> World {
    let name = format!("atln{tag}-{}", std::process::id());
    for base in [std::env::temp_dir(), PathBuf::from("/tmp")] {
        let root = base.join(&name);
        if !launch_isolation::sock_fits(&root.join("run/aterm/lanes.sock.token")) {
            continue;
        }
        if launch_isolation::prepare(&root).is_ok() {
            return World(root);
        }
        let _ = std::fs::remove_dir_all(&root);
    }
    could_not_run("no scratch base with a socket path short enough for sun_path")
}

/// Boot a headless instance on `sock` under a soft descriptor limit of
/// `soft_limit` (the hard limit kept; COULD NOT RUN when it cannot have it).
/// Returns the instance and the limit it runs under. An instance that exits,
/// or says nothing of its socket within [`HANG`], fails the test.
fn boot(root: &Path, sock: &Path, soft_limit: u64) -> (Reaped, u64) {
    let (_, hard) = descriptor_limits();
    if hard < soft_limit {
        could_not_run(format!(
            "the hard descriptor limit {hard} is below the {soft_limit} this test needs"
        ));
    }
    let log_path = root.join("instance.log");
    let log = std::fs::File::create(&log_path)
        .and_then(|log| Ok((log.try_clone()?, log)))
        .unwrap_or_else(|e| could_not_run(format!("cannot open the instance log ({e})")));
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_aterm"));
    launch_isolation::apply(&mut cmd, root);
    cmd.args(["--headless", launch_isolation::NO_REROUTE])
        .arg("--control-sock")
        .arg(sock)
        .stdin(Stdio::null())
        .stdout(log.0)
        .stderr(log.1);
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
    let mut ready = aterm_agent::ready::Ready::open(root)
        .unwrap_or_else(|e| panic!("arm the ready signal in {}: {e}", root.display()));
    ready.hand(&mut cmd);
    let mut child = match cmd.spawn() {
        Ok(child) => Reaped(child, Some(lifeline)),
        Err(e) => could_not_run(format!("cannot launch aterm --headless ({e})")),
    };
    let said = ready.wait(HANG);
    if !matches!(said, aterm_agent::ready::Signal::Ready) {
        let _ = child.0.kill();
        let status = child.0.wait();
        panic!(
            "the instance did not say its socket at {} listens ({said:?}, then {status:?}); \
             log:\n{}",
            sock.display(),
            std::fs::read_to_string(&log_path).unwrap_or_default()
        );
    }
    (child, soft_limit)
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

    /// Dial, authenticate and have one `version` answered. A refusal while the request lanes
    /// are briefly full of work (a burst of connections authenticating on a loaded machine) is
    /// the busy line clients retry on, so it is retried; one that lasts fails at the hang
    /// detector with the server's last words.
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

    /// A reply already waiting on the connection, read without blocking: the listener writes
    /// its busy line before it accepts the next peer, so once a later peer is answered, a
    /// refused connection holds that line.
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
            Err(e) if hung_up(&e) => Some("<EOF>".to_string()),
            Err(e) => Some(format!("<read error: {e}>")),
        }
    }

    fn line(&mut self) -> String {
        let mut reply = String::new();
        match self.reader.read_line(&mut reply) {
            Ok(0) => "<EOF>".to_string(),
            Ok(_) => reply.trim_end().to_string(),
            Err(e) if hung_up(&e) => "<EOF>".to_string(),
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

/// A fresh one-shot client: AUTH and `version` in one write (as `aterm-ctl` sends them); the
/// first reply line, even after a failed write. Reads are bounded at [`HANG`], so a client
/// queued rather than answered reads an error, not a late `OK`.
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
        Err(e) if hung_up(&e) => "<EOF>".to_string(),
        Err(e) => format!("<read error: {e}>"),
    }
}

/// Whether a read error is the listener's close. Linux answers a read on a
/// connection closed with input still unread (a wait refused busy, its request
/// line never read) with `ECONNRESET` once, then end-of-file; macOS with
/// end-of-file. `aterm ctl` reads both as a hang-up (`is_hangup`).
fn hung_up(e: &std::io::Error) -> bool {
    e.kind() == std::io::ErrorKind::ConnectionReset
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

/// [`idle_driver`] below the socket's cap: a busy line there is a moment (a
/// lane still placing an earlier connection), dialled again until served — a
/// [`HANG`] detector. `Err` with the refusal when a busy line never clears,
/// or at once when a refusal is not the busy line.
fn idle_driver_below_cap(sock: &Path, token: &str) -> Result<Driver, String> {
    aterm_spec::wait::within(HANG, Duration::from_millis(50), || {
        match idle_driver(sock, token) {
            Err(again) if again == BUSY => None,
            answer => Some(answer),
        }
    })
    .unwrap_or_else(|| Err(BUSY.to_string()))
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

/// One wait the socket admitted, on a fresh connection, and the probe that followed it:
/// `(wait, probe reply)`. `reopened` counts waits refused on arrival and opened again.
///
/// A request lane that has just answered still counts busy for a moment, so a wait can be
/// refused on arrival. That is told apart without a timer: the listener takes connections in
/// order and writes its busy line before it accepts the next, so once the following probe is
/// answered a refused wait already holds the line, then end-of-file, and is opened again. A
/// wait a lane told busy keeps its connection open with nothing after the line, and fails
/// here. One connection at a time, never a burst.
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

/// A fresh client's answer below the socket's limits: a busy probe is a moment, probed again
/// until it clears. It stays [`BUSY`] only if it never clears within [`HANG`], a hang detector,
/// never a latency budget.
fn answered(sock: &Path, token: &str) -> String {
    let mut reply = probe(sock, token);
    let deadline = Instant::now() + HANG;
    while reply == BUSY && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
        reply = probe(sock, token);
    }
    reply
}

#[test]
fn idle_and_waiting_drivers_do_not_starve_a_new_client() {
    const IDLE: usize = 12;
    const WAITING: usize = 12;
    /// The waits' own timeout: a fresh client queued behind them would be
    /// answered only when they end, far past a probe's [`HANG`] read bound.
    const WAIT_MS: u64 = 180_000;

    roomy_test_process();
    let world = world("s");
    let sock = world.0.join("run/aterm/lanes.sock");
    // The budget of the window the owner starts from the Dock.
    let _instance = boot(&world.0, &sock, LAUNCHD_SOFT_LIMIT);
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
        // A busy line here is a lane still placing the wait: a moment.
        let reply = if reply == BUSY {
            answered(&sock, &token)
        } else {
            reply
        };
        assert!(
            reply.starts_with("OK"),
            "a fresh client was refused with {IDLE} idle and {} waiting drivers open: \
             {reply:?}",
            index + 1
        );
        waiting.push(wait);
    }

    // Twenty-four persistent drivers against eight request lanes: every fresh client is still
    // answered (below the limits a busy line is a moment, waited out).
    for attempt in 0..20 {
        let reply = answered(&sock, &token);
        assert!(
            reply.starts_with("OK"),
            "fresh client {attempt} was refused with {IDLE} idle and {WAITING} waiting \
             drivers open: {reply:?}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    // The CLI path a person types.
    let out = aterm_ctl(&world.0, &sock, &["status"]);
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
fn the_connection_past_the_cap_takes_the_stalest_place_and_tabs_still_open() {
    roomy_test_process();
    let world = world("c");
    let sock = world.0.join("run/aterm/lanes.sock");
    let (_instance, soft_limit) = boot(&world.0, &sock, LAUNCHD_SOFT_LIMIT);
    let cap = connection_cap(soft_limit);
    let token = token(&sock);

    // Idle drivers, each served once and kept open, up to the cap. Below the cap a busy line is
    // always a moment (on a loaded machine every request lane can still be placing an earlier
    // connection, and a refused one never counts), so it is dialled again until served; a cap
    // below the documented one never lets a retry through and fails at [`HANG`] naming where
    // it held.
    let mut held: Vec<Driver> = Vec::new();
    let mut cleared = 0;
    while held.len() < cap {
        let refused = match idle_driver(&sock, &token) {
            Ok(driver) => {
                held.push(driver);
                continue;
            }
            Err(refused) => refused,
        };
        assert_eq!(
            refused,
            BUSY,
            "below the cap ({} of {cap} open, soft descriptor limit {soft_limit}) a refusal \
             is the busy line, not a process out of descriptors",
            held.len()
        );
        cleared += 1;
        assert!(
            cleared <= 40,
            "{cleared} refusals cleared on retry below the cap: the lanes are not free of \
             idle connections"
        );
        let open = held.len();
        let driver = idle_driver_below_cap(&sock, &token).unwrap_or_else(|reply| {
            panic!(
                "refused at {open} open ({reply:?}): the cap is below a quarter of the soft \
                 descriptor limit {soft_limit} ({cap}), the lanes are held by idle connections \
                 ({cleared} momentary refusals cleared before), or the refusal is not the busy \
                 line"
            )
        });
        held.push(driver);
    }
    eprintln!(
        "cap: {} open at soft limit {soft_limit}; {cleared} momentary refusals retried",
        held.len()
    );
    // At the cap the connection past it is served — never refused while drivers sit parked —
    // in the place of the stalest: the driver served longest ago, closed for it, so the cap is
    // never passed. The others are untouched.
    let fresh = idle_driver(&sock, &token).unwrap_or_else(|refused| {
        panic!(
            "the connection past {cap} open idle ones (soft descriptor limit {soft_limit}) was \
             refused: {refused:?}"
        )
    });
    assert_eq!(
        held[0].waiting_reply().as_deref(),
        Some("<EOF>"),
        "the stalest driver, closed for the connection past the cap"
    );
    for (index, driver) in held.iter_mut().enumerate().skip(1) {
        assert_eq!(
            driver.waiting_reply(),
            None,
            "held driver {index}, untouched"
        );
    }
    held.remove(0);
    held.push(fresh);

    // The window still has descriptors for what it is for: new tabs.
    let newest = held.len() - 1;
    for index in 0..3 {
        let reply = held[newest].request("spawn");
        assert!(
            reply.starts_with("OK s-"),
            "tab {index} could not open with {cap} control connections held: {reply:?}"
        );
    }
    // `aterm ctl` at the cap is answered, for a verb it sends (`status`) and for the discovery
    // it answers itself (`ls`): each connection it makes there takes the stalest's place.
    for verb in ["status", "ls"] {
        let out = aterm_ctl(&world.0, &sock, &[verb]);
        assert!(
            out.status.success(),
            "aterm ctl {verb} at the cap: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    // The drivers it displaced are the stalest, oldest first, and no others: every driver
    // still open is served, in order, on the connection it holds.
    let mut displaced = 0;
    while displaced < held.len() && held[displaced].waiting_reply().as_deref() == Some("<EOF>") {
        displaced += 1;
    }
    assert!(
        displaced >= 1,
        "aterm ctl at the cap displaced no driver: it was served past the cap"
    );
    for (index, driver) in held.iter_mut().enumerate().skip(displaced) {
        let reply = driver.request("version");
        assert!(
            reply.starts_with("OK"),
            "held driver {index} ({displaced} stalest displaced before it): {reply:?}"
        );
    }
    eprintln!("past the cap: 1 + {displaced} stalest drivers displaced, the rest served");

    // At a cap where nothing gives way — every connection held on a wait, none parked — a fresh
    // client is refused at admission, and `aterm ctl` reports the server's own busy line, for a
    // verb it sends (`status`) and for the discovery it answers itself (`ls`): never the write's
    // `Broken pipe`, never "did not answer".
    drop(held);
    let mut reopened = 0;
    let mut waits = Vec::new();
    while waits.len() < cap {
        waits.push(admitted_wait(&sock, &token, 180_000, &mut reopened).0);
    }
    // A probe answered just before the last wait landed may still sit parked for a moment, and
    // gives way to the next client: the cap of waits refuses once it is gone (a hang detector).
    let refused = aterm_spec::wait::within(HANG, Duration::from_millis(50), || {
        (probe(&sock, &token) == BUSY).then_some(())
    });
    assert!(
        refused.is_some(),
        "a fresh client at a cap of {cap} waits was never refused"
    );
    for verb in ["status", "ls"] {
        let out = aterm_ctl(&world.0, &sock, &[verb]);
        let said = String::from_utf8_lossy(&out.stderr);
        assert!(
            !out.status.success(),
            "aterm ctl {verb} succeeded at a cap of waits: {said}"
        );
        assert!(
            said.contains(BUSY),
            "aterm ctl {verb} at a cap of waits must say {BUSY:?}; it said: {said}"
        );
        for wrong in ["did not answer", "Broken pipe"] {
            assert!(
                !said.contains(wrong),
                "aterm ctl {verb} at a cap of waits said {wrong:?}: {said}"
            );
        }
    }
    eprintln!("a cap of {cap} waits: refused at admission ({reopened} waits opened again)");
    drop(waits);
}

/// `aterm ctl --sock <sock> <args…>` in the world's isolation, asked again, a fresh client
/// each time, while the instance has not yet accepted it ([`ctl_accept::until_accepted`]): the
/// client's own 10 s accept budget is a person's, and a loaded machine can miss it. A busy
/// refusal is an answer, returned at once.
fn aterm_ctl(root: &Path, sock: &Path, args: &[&str]) -> std::process::Output {
    ctl_accept::until_accepted(|| {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_aterm"));
        launch_isolation::apply(&mut cmd, root);
        cmd.arg("ctl")
            .arg("--sock")
            .arg(sock)
            .args(args)
            .stdin(Stdio::null())
            .output()
            .expect("run aterm ctl")
    })
}

#[test]
fn waits_past_their_lanes_are_told_busy_and_hung_up_waits_give_their_places_back() {
    // Held idle alongside the waits: they must not count.
    const IDLE: usize = 16;
    // Waits past every lane a wait may take.
    const PAST: usize = 4;
    /// Far longer than the test, so only a hangup can end these waits in time (checked below):
    /// a wait that ended on its own would pass the drain vacuously.
    const WAIT_MS: u64 = 180_000;

    roomy_test_process();
    let world = world("h");
    let sock = world.0.join("run/aterm/lanes.sock");
    // Room for every wait plus the idle drivers: the lanes are what is judged
    // here, not the connection cap.
    let (_instance, soft_limit) = boot(&world.0, &sock, ROOMY_SOFT_LIMIT);
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

    // The CLI path a person types, beside every wait: a request is answered, and a wait no
    // lane may take is a plain failure carrying the server's own words.
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

    // Every waiting client hangs up, as a SIGKILLed `aterm ctl` does: their places come back
    // at the hangup, not at their waits' timeouts, so a fresh wait is held and runs to its own
    // short timeout. Every wait must still be open through the whole drain, or one ending on
    // its own would free its place with no hangup.
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

/// A wait with `version` right behind it on its own connection, in one write or, with
/// `relay_shape`, as `RelayClient::write_line` writes (each line, then its newline). `Ok` when
/// the socket holds it; `Err` with what it said when a lane told it busy and served the
/// `version`. A connection refused on arrival is dialled again, counted in `reopened`; the
/// probe after it orders the check (the listener writes its busy line before it accepts the
/// next peer).
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

/// One wait that ends on its own soon, with `version` behind it in the same write:
/// `(wait, version)`. The carry holds for a wait answered by its timeout as for one answered by
/// an event, and no clock judges it. A connection refused on arrival (the busy line, then
/// end-of-file) is dialled again; a wait a lane told busy keeps its connection, serves the
/// `version` behind it, and is reported.
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
    // Twice the request lanes, so far past their wait budget.
    const PIPELINED: usize = 2 * RPC_LANES;
    /// The waits' own timeout: a hang bound they never reach. They end by the
    /// event they wait for (the text typed below), so the test takes as long as
    /// the server takes, not as long as a wait's clock.
    const WAIT_MS: u64 = 180_000;

    roomy_test_process();
    let world = world("p");
    let sock = world.0.join("run/aterm/lanes.sock");
    let (_instance, soft_limit) = boot(&world.0, &sock, LAUNCHD_SOFT_LIMIT);
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
    // Nothing has happened to any held wait yet, so none may have an answer waiting; otherwise
    // "held" would be vacuous.
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
    /// Of each kind: twice the request lanes.
    const EACH: usize = 2 * RPC_LANES;

    roomy_test_process();
    let world = world("m");
    let sock = world.0.join("run/aterm/lanes.sock");
    let _instance = boot(&world.0, &sock, ROOMY_SOFT_LIMIT);
    let token = token(&sock);
    // What a `help --full` answer is, read whole by a client that reads.
    let whole = {
        let mut reader = Driver::open(&sock, &token);
        reader.send("help --full");
        reader.send("version");
        read_through(&mut reader.reader, "OK version=").0
    };

    // One peer at a time, never a burst (a burst of fresh connections is work that may fill
    // the request lanes for a moment, which the busy line is for). A silent peer the listener
    // refused on arrival holds the busy line once a later client is answered, and is dialled
    // again.
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

/// THE TWO WAYS A LIVE TEST ASKS, on answers made here: [`ctl_accept::until_served`] (every
/// other live test's) asks again on a missed accept and on the busy line, neither of which
/// served anything, and returns the first answer that is neither, a failure included;
/// [`ctl_accept::until_accepted`] (this file's, whose verdicts are the busy line) returns the
/// busy line as it stands.
#[test]
fn a_busy_answer_is_asked_again_where_it_is_not_the_verdict() {
    use std::os::unix::process::ExitStatusExt as _;
    use std::process::{ExitStatus, Output};
    let answer = |code: i32, stderr: &str| Output {
        status: ExitStatus::from_raw(code << 8),
        stdout: Vec::new(),
        stderr: stderr.as_bytes().to_vec(),
    };
    let missed = || {
        answer(
            2,
            "aterm-ctl: no answer from /s within 10s: the instance took no request, so none was sent\n",
        )
    };
    let busy = || answer(1, &format!("aterm-ctl: {BUSY}\n"));
    let refused = || answer(1, "aterm-ctl: ERR unknown verb\n");
    let served = || answer(0, "");

    let mut script = vec![missed(), busy(), busy(), served()].into_iter();
    let mut asks = 0;
    let out = ctl_accept::until_served(|| {
        asks += 1;
        script.next().expect("asked past the last answer")
    });
    assert_eq!((asks, out.status.code()), (4, Some(0)));

    let mut script = vec![busy(), refused()].into_iter();
    let mut asks = 0;
    let out = ctl_accept::until_served(|| {
        asks += 1;
        script.next().expect("asked past the last answer")
    });
    assert_eq!(asks, 2);
    assert_eq!(
        out.stderr,
        refused().stderr,
        "another refusal is the answer"
    );

    let mut script = vec![missed(), busy(), served()].into_iter();
    let mut asks = 0;
    let out = ctl_accept::until_accepted(|| {
        asks += 1;
        script.next().expect("asked past the last answer")
    });
    assert_eq!(asks, 2);
    assert!(ctl_accept::was_turned_away(&out), "{out:?}");
    assert!(!ctl_accept::was_turned_away(&served()) && !ctl_accept::was_turned_away(&missed()));
}
