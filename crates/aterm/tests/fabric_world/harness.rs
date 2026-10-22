// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Shared by the `fabric_world` suite: one guarded broker, one headless aterm,
//! one real bridge child between them, and the bounded waits that keep the
//! whole thing deterministic.
//!
//! Both suites mount this file, so it carries NO `#[test]` of its own: one
//! would run once per mounting binary.
//!
//! NOTHING HERE SLEEPS AS SYNCHRONISATION. Every wait is `until <observable
//! state>`, bounded by [`DEADLINE`] — and that bound is a HANG DETECTOR, not a
//! performance assertion: a healthy step here takes single-digit milliseconds,
//! and the bound only turns a wedged run into a diagnosable failure instead of a
//! `cargo test` that never returns.

#![allow(dead_code)]
#![allow(
    clippy::disallowed_types,
    reason = "test code: a stand-in child may inherit descriptors"
)]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use aterm_uds::spawnfd::{Child as OwnedChild, Stdio as OwnedStdio};

use astream_broker::{Broker, BrokerHandle, Client, OpenOptions, Prepared};
use aterm_link::ctl::{Ctl, Reply};

/// The mint secret every test's broker is guarded with. Thirty-two bytes, a
/// mint secret's floor (`aterm_link::enable::SECRET_LEN`), so a world may also
/// run its broker as a real `aterm link broker --secret-file` process
/// (`broker_poison`'s), which refuses a shorter one, under the same caps.
pub const SECRET: &[u8] = b"a3-bridge-e2e-secret-of-32-bytes";

/// THE OPTIONS A TEST BROKER OPENS WITH: exactly aterm's own for `tier`
/// ([`aterm_link::durability::Durability::options`]: astream's tier and, on
/// a weak one, aterm's escalation table), guarded by [`SECRET`] as every
/// broker `aterm fabric on` runs is. A test or the bench that measures a
/// horizon of its own sets `durability` after this, keeping the table.
#[must_use]
pub fn broker_opts(tier: aterm_link::durability::Durability) -> OpenOptions {
    let mut opts = tier.options();
    opts.guard = Some(SECRET.to_vec());
    opts
}

/// Open a guarded broker on `log` with `opts` ([`broker_opts`]), RETAINED by
/// aterm's default rules ([`aterm_link::broker_rules::retain`], session log
/// Phase 6 step 6E-2, ruling U24): the one constructor `aterm link broker`
/// opens with too, so a world's broker is the production default. The rules
/// are set per open, as `aterm link broker` sets them at every start.
///
/// # Panics
///
/// The open, or the rules, failed.
#[must_use]
pub fn open_broker(log: impl AsRef<Path>, opts: &OpenOptions) -> Broker {
    let broker = open_broker_unretained(log, opts);
    aterm_link::broker_rules::retain(&broker).expect("the default retention rules");
    broker
}

/// [`open_broker`] with NO retention rules: a broker from before astream's R4,
/// which never compacts and advertises no rule ([`World::without_retention`]).
///
/// # Panics
///
/// The open failed.
#[must_use]
pub fn open_broker_unretained(log: impl AsRef<Path>, opts: &OpenOptions) -> Broker {
    Broker::open_opts(log, opts.clone(), || Ok(Prepared::Nothing)).expect("guarded broker")
}

/// The fleet every test runs in.
pub const FLEET: &str = "f1";

/// The ceiling on every wait. Generous on purpose — see the module note.
pub const DEADLINE: Duration = aterm_spec::wait::MUST_HAPPEN;

/// `sockaddr_un.sun_path` is ~104 bytes on macOS/BSD. Every path here is built
/// short enough with margin, and [`assert_sock_fits`] is the check that says so
/// out loud rather than failing deep inside `bind` or `connect`.
const MAX_SOCK_PATH: usize = 100;

/// Assert that a socket at `sock` fits `sun_path` as a client that resolves it
/// dials it: its directory (which exists) canonicalized, as `/tmp` is
/// `/private/tmp` on macOS.
fn assert_sock_fits(sock: &str) {
    let p = Path::new(sock);
    let dialled = match (p.parent().map(std::fs::canonicalize), p.file_name()) {
        (Some(Ok(dir)), Some(name)) => dir.join(name),
        _ => p.to_path_buf(),
    };
    assert!(
        dialled.as_os_str().len() < MAX_SOCK_PATH,
        "socket path too long for sun_path as dialled: {}",
        dialled.display()
    );
}

/// Wait until `f` answers `Some`, or fail with `what`.
///
/// # Panics
///
/// After [`DEADLINE`], naming what was being waited for — a hang detector.
pub fn until<T>(what: &str, f: impl FnMut() -> Option<T>) -> T {
    until_within(DEADLINE, what, f)
}

/// [`until`] with a stated budget, for the one class of observable that has a
/// PERIOD of its own.
///
/// Most of what these tests wait for is published by the arrival that caused it,
/// so [`DEADLINE`] is 60 s of pure slack. A few things are published by the
/// bridge's periodic backstop instead — §6.6's conservative pause and local
/// lease mirror, and `attention=` — because they are discovered by LOOKING, and
/// `bridge.rs`'s `ROSTER_REFRESH` is their period. A wait on one of those is a
/// wait on `<the machine> + <up to one full round>`, and giving it the same
/// budget as an event-driven wait is how a slow machine turns a correct bridge
/// into a red test.
///
/// USE IT ONLY WHERE THE PERIOD IS REAL, and say which one in the call. It is
/// still a hang detector: nothing here sleeps as synchronisation, and a bridge
/// that never publishes still fails, just later.
pub use aterm_spec::wait::until_within;

/// The budget for something the bridge's periodic backstop publishes.
///
/// `bridge.rs`'s `ROSTER_REFRESH` is 2 s and the round is a bounded local walk,
/// so the honest bound is a handful of rounds plus whatever the machine is
/// doing. THREE MINUTES IS NOT A PERFORMANCE CLAIM — a healthy round lands in
/// milliseconds. It is [`DEADLINE`]'s argument applied to a slower clock: the
/// only thing the number decides is how long a WEDGED run takes to be named.
pub const PERIODIC_DEADLINE: Duration = Duration::from_secs(180);

/// The fast-timers development seam as a [`World::boot_with`] environment
/// entry: the instance's own waits at test pace (`aterm-gui-timing`), and its
/// bridge's round and the presence republish window matched to it a quarter of
/// production (`aterm-link`'s `roster_refresh`, `presence::republish_min`).
///
/// FOR A TEST THAT WAITS ON THE ROUND: a presence field a push cannot carry
/// inside the republish window, the roster backstop itself, a replay walked per
/// round. NEVER for one whose proof is that NO round ran inside some window (a
/// premise made by posting before the next round, a bug a round would mask):
/// four rounds a second are four times as likely to fall inside it.
pub const FAST_TIMERS: (&str, &str) = (aterm_types::domain::ENV_DEV_FAST_TIMERS, "1");

/// The bound on ONE control-verb reply: [`DEADLINE`] for the verb's own wait
/// (`--wait=60000`, `--wait-ack=60000`, `timeout=60000` — each a minute's hang
/// detector of its own) plus [`DEADLINE`] for the reply. A HANG DETECTOR like
/// every other bound here: an instance that accepts a connection and never
/// answers fails the test naming the verb, instead of holding its live slot for
/// ever.
pub const VERB_DEADLINE: Duration = Duration::from_secs(2 * DEADLINE.as_secs());

/// Connect to an instance's control socket and authenticate, with every read
/// and write on the connection bounded by `bound`. `Ctl`'s reader half is a
/// `dup` of the same socket, so the socket's timeouts bound it too.
///
/// # Errors
///
/// The connect, the write of the token, or setting a timeout.
pub fn connect_within(sock: &str, token: &str, bound: Duration) -> std::io::Result<Ctl> {
    let ctl = Ctl::connect(sock, token)?;
    ctl.get_ref().set_read_timeout(Some(bound))?;
    ctl.get_ref().set_write_timeout(Some(bound))?;
    Ok(ctl)
}

/// [`connect_within`] at [`VERB_DEADLINE`] — the connection every test verb goes
/// over.
///
/// # Errors
///
/// As [`connect_within`].
pub fn connect(sock: &str, token: &str) -> std::io::Result<Ctl> {
    connect_within(sock, token, VERB_DEADLINE)
}

/// How many full stacks may be COMING UP at once inside one test binary.
///
/// Every e2e test here boots a broker, a headless `aterm` and the real
/// `aterm link serve` child it launches, and `cargo test` runs the tests
/// in a binary in parallel with no `--test-threads` limit anywhere in the
/// workspace. On a machine with cores to spare that is fine; beside a second
/// cargo job it is not, and the failure it produces is a 60 s timeout on a
/// healthy step — a red test that says nothing about the code under it, which is
/// exactly what this harness's header refuses to accept.
///
/// So the BOOTS are capped and nothing else is: the tests still run in parallel,
/// and only the interval where three processes are starting at once is
/// serialised. Two at a time on any machine, up to four where there are cores
/// for them.
fn max_concurrent_boots() -> usize {
    std::thread::available_parallelism().map_or(2, |n| (n.get() / 3).clamp(2, 4))
}

/// The gate [`boot_permit`] hands out. `usize` is the number of boots in flight.
fn boot_gate() -> &'static (std::sync::Mutex<usize>, std::sync::Condvar) {
    static GATE: std::sync::OnceLock<(std::sync::Mutex<usize>, std::sync::Condvar)> =
        std::sync::OnceLock::new();
    GATE.get_or_init(|| (std::sync::Mutex::new(0), std::sync::Condvar::new()))
}

/// A permit to bring a whole stack up. Held for as long as the guard lives, and
/// released on drop — including on the unwind of a failing boot, so one panicking
/// test cannot wedge every other test in the binary behind a permit it never
/// gave back.
#[must_use]
pub struct BootPermit;

impl Drop for BootPermit {
    fn drop(&mut self) {
        let (lock, cv) = boot_gate();
        let mut n = lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *n = n.saturating_sub(1);
        cv.notify_one();
    }
}

/// How many TESTS may hold live stacks at once inside one test binary.
///
/// [`max_concurrent_boots`] bounds the interval where stacks are coming up; this
/// bounds how many are UP. In one binary every stack test is eligible at once,
/// up to libtest's thread count, beside scenarios that hold an outage open or
/// let a deadline pass on the wall clock (`receipt_edge_cases.rs`). Measured at
/// load ~40 on 18 cores (2026-09-23): uncapped, 12 headless guis at peak and a
/// red `exactly_once::post_key_is_exactly_once_across_a_bridge_and_a_broker_restart`
/// ("ERR fabric stalled") that passes alone; capped, 9 at peak and 3 of 3 green.
/// So the cap is half the cores, never more than 11 (the most any one suite
/// file holds). That particular red was the load exposing a race in the test's
/// own broker restart, a `connected` read before the bridge saw the outage
/// (reproduced with a half-second-late EOF, closed by [`World::stop_broker`]);
/// the cap stays for what load does to every other wait.
fn max_live_stack_tests() -> usize {
    std::thread::available_parallelism().map_or(2, |n| (n.get() / 2).clamp(2, 11))
}

/// The gate [`take_live_slot`] hands out: how many tests hold a slot, and who waits.
///
/// SERVED IN NAME ORDER, NOT WAKE ORDER. A bare condvar wakes whichever waiter
/// the scheduler picks, so at the binary's start the tests libtest launches
/// together (one per test thread) raced for the slots, and the suite's long
/// pole — sorted first on purpose (`fabric_world/main.rs`) — waited 3.1 s of a
/// 35.0 s run behind tests started beside it. libtest names each
/// test's thread after the test, so a slot goes to the waiting test whose name
/// sorts first: the order libtest starts the tests in is the order they get
/// stacks.
#[derive(Default)]
struct LiveGate {
    /// Tests holding a slot.
    held: usize,
    /// `(thread name, arrival)` of every test waiting for one, least served
    /// first; the arrival keeps two unnamed threads apart.
    waiting: std::collections::BTreeSet<(String, u64)>,
    arrivals: u64,
}

fn live_gate() -> &'static (std::sync::Mutex<LiveGate>, std::sync::Condvar) {
    static GATE: std::sync::OnceLock<(std::sync::Mutex<LiveGate>, std::sync::Condvar)> =
        std::sync::OnceLock::new();
    GATE.get_or_init(Default::default)
}

/// A test thread's claim on one of [`max_live_stack_tests`] slots.
///
/// PER TEST, NOT PER STACK: a test that boots two worlds, or a fleet's second
/// node, takes one slot for all of them, so a test holding a slot never waits on
/// another slot (no test can wedge the binary half-booted). The slot lives in a
/// thread-local and is released when the thread exits — libtest runs each test
/// on its own thread, and a panicking test's thread still exits, so a failure
/// gives its slot back.
struct LiveSlot;

impl Drop for LiveSlot {
    fn drop(&mut self) {
        let (lock, cv) = live_gate();
        let mut gate = lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        gate.held = gate.held.saturating_sub(1);
        // ALL: only the waiting name that sorts first may take it, and it is not
        // the one a `notify_one` would pick.
        cv.notify_all();
    }
}

thread_local! {
    static LIVE_SLOT: std::cell::RefCell<Option<LiveSlot>> =
        const { std::cell::RefCell::new(None) };
}

/// A test's place in the [`LiveGate`] queue, taken by [`queue_for_live_slot`]
/// and given back when dropped — so a test that unwinds before its turn never
/// leaves a name the rest of the binary waits behind.
struct Queued(Option<(String, u64)>);

impl Drop for Queued {
    fn drop(&mut self) {
        if let Some(me) = self.0.take() {
            let (lock, cv) = live_gate();
            let mut gate = lock
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            gate.waiting.remove(&me);
            cv.notify_all();
        }
    }
}

/// Join the live-slot queue under this test's name, or `None` when the calling
/// test already holds a slot (idempotent per thread).
fn queue_for_live_slot() -> Option<Queued> {
    if LIVE_SLOT.with(|s| s.borrow().is_some()) {
        return None;
    }
    let (lock, _) = live_gate();
    let mut gate = lock
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    gate.arrivals += 1;
    let me = (
        std::thread::current()
            .name()
            .unwrap_or_default()
            .to_string(),
        gate.arrivals,
    );
    gate.waiting.insert(me.clone());
    Some(Queued(Some(me)))
}

/// Wait for the queued test's turn and a free slot ([`max_live_stack_tests`]),
/// then hold it for the rest of the test.
fn take_live_slot(mut queued: Queued) {
    let me = queued.0.take().expect("a queued test holds its place");
    let (lock, cv) = live_gate();
    let cap = max_live_stack_tests();
    let mut gate = lock
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    while gate.held >= cap || gate.waiting.first() != Some(&me) {
        gate = cv
            .wait(gate)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
    }
    gate.waiting.remove(&me);
    gate.held += 1;
    drop(gate);
    // The next name in order may find a slot still free.
    cv.notify_all();
    LIVE_SLOT.with(|s| *s.borrow_mut() = Some(LiveSlot));
}

/// Take a boot permit, waiting for one if [`max_concurrent_boots`] are already
/// out. The calling test first takes its live slot ([`take_live_slot`]) —
/// always before a boot permit, never while holding one, so the two gates
/// cannot deadlock.
pub fn boot_permit() -> BootPermit {
    if let Some(queued) = queue_for_live_slot() {
        take_live_slot(queued);
    }
    let (lock, cv) = boot_gate();
    let cap = max_concurrent_boots();
    let mut n = lock
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    while *n >= cap {
        n = cv
            .wait(n)
            .unwrap_or_else(std::sync::PoisonError::into_inner);
    }
    *n += 1;
    BootPermit
}

/// A child killed and reaped when dropped, so a bounded wait on it that fails
/// never leaves it running. A kill after a normal exit is a no-op: `Child`
/// knows it was reaped.
struct Reaped(Child);

impl Drop for Reaped {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Read `pipe` to its end on a thread of its own, so the read can be bounded.
fn drain(mut pipe: impl std::io::Read + Send + 'static) -> std::sync::mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = pipe.read_to_end(&mut buf);
        let _ = tx.send(buf);
    });
    rx
}

/// `cmd.output()` with its waits bounded by [`DEADLINE`] — the exit, and then
/// each pipe's end — for a child CLI a test runs to completion. stdin is
/// `/dev/null`, as `output` makes it.
///
/// `Command::output` waits for ever: a CLI that wedged parked its test, and the
/// live slot that test held, for the rest of the run. Every wait the CLIs these
/// suites drive make of their own is seconds, so a minute is pure slack; a
/// child still running at the deadline is killed and the test fails naming
/// `what`.
///
/// # Panics
///
/// The spawn failed, the child outlived [`DEADLINE`], or a pipe stayed open a
/// [`DEADLINE`] past its exit (a process it started still holds it).
pub fn output_within(cmd: &mut Command, what: &str) -> std::process::Output {
    let child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("spawn {what}: {e}"));
    wait_output_within(child, what)
}

/// `child.wait_with_output()` with its waits bounded by [`DEADLINE`], as
/// [`output_within`] bounds them: for a child the test spawned itself because
/// it needed more than `output` gives (a piped stdin it writes and closes
/// first). A piped stdout or stderr is read to its end; one not piped answers
/// empty. A child still running at the deadline is killed and the test fails
/// naming `what`.
///
/// # Panics
///
/// The child outlived [`DEADLINE`], or a pipe stayed open a [`DEADLINE`] past
/// its exit (a process it started still holds it).
pub fn wait_output_within(child: Child, what: &str) -> std::process::Output {
    let mut child = Reaped(child);
    let stdout = child.0.stdout.take().map(drain);
    let stderr = child.0.stderr.take().map(drain);
    let status = until(&format!("{what} to exit"), || {
        child.0.try_wait().ok().flatten()
    });
    let read = |rx: Option<std::sync::mpsc::Receiver<Vec<u8>>>, pipe: &str| {
        rx.map_or_else(Vec::new, |rx| {
            rx.recv_timeout(DEADLINE).unwrap_or_else(|_| {
                panic!("{what} exited ({status}) but its {pipe} stayed open past {DEADLINE:?}")
            })
        })
    };
    std::process::Output {
        status,
        stdout: read(stdout, "stdout"),
        stderr: read(stderr, "stderr"),
    }
}

/// A broker client on `sock` whose every read and write is bounded by
/// [`VERB_DEADLINE`] — a HANG DETECTOR, like [`connect`]'s: a broker that
/// accepts and never answers fails the test at the call that waited, instead
/// of parking it, and the live slot it holds, for ever.
///
/// # Panics
///
/// The connect, or setting a timeout, failed.
pub fn broker_client(sock: impl AsRef<Path>) -> Client {
    let sock = sock.as_ref();
    let s = std::os::unix::net::UnixStream::connect(sock)
        .unwrap_or_else(|e| panic!("connect to the broker at {}: {e}", sock.display()));
    s.set_read_timeout(Some(VERB_DEADLINE))
        .and_then(|()| s.set_write_timeout(Some(VERB_DEADLINE)))
        .expect("bound the broker connection");
    Client::from_stream(s)
}

/// The first line `child` prints on its (piped) stdout — a readiness line —
/// bounded by [`DEADLINE`]. A child that prints nothing by then is killed and
/// the test fails naming `what`; one that exits first answers an empty line,
/// which the caller's assertion on the line then names.
///
/// The pipe is CLOSED after that one line, the way a supervisor's readiness
/// probe may close it: a child that dies writing a later line has a bug of its
/// own (`fabric_cli::a_broker_whose_stdout_reader_left_keeps_serving`).
///
/// # Panics
///
/// The child's stdout was not piped, or no line came within [`DEADLINE`].
pub fn first_line_within(child: &mut Child, what: &str) -> String {
    use std::io::BufRead;
    let out = child.stdout.take().expect("the child's stdout is piped");
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let _ = std::io::BufReader::new(out).read_line(&mut line);
        let _ = tx.send(line);
    });
    rx.recv_timeout(DEADLINE).unwrap_or_else(|_| {
        let _ = child.kill();
        let _ = child.wait();
        panic!("timed out after {DEADLINE:?} waiting for: {what}")
    })
}

/// Mint one capability and render it as a `--cap-file` line.
pub fn cap_line(grant: &str) -> String {
    let cap = astream_cap::mint(SECRET, grant).expect("mint");
    let hex: String = cap.tag.iter().map(|b| format!("{b:02x}")).collect();
    format!("{} {hex}\n", cap.filter)
}

/// A node's ring on the test fleet: THE ring `fabric on` mints
/// ([`aterm_link::enable::node_grants`]), so every guarded World and Fleet
/// bridge attaches with what ships, and a grant the bridge needs cannot be
/// added here and forgotten there.
pub fn node_grants(node: &str) -> Vec<String> {
    aterm_link::enable::node_grants(FLEET, node)
}

/// THE INHERITED IDENTITY MUST BE STRIPPED, and this is not hygiene — it is the
/// difference between a two-node test and a one-node test wearing two hats.
///
/// aterm's ROOT session ADOPTS `$ATERM_SESSION_ID` / `$ATERM_LAUNCH_NONCE` when
/// they are set (`spawn::adopt_injected_identity`), which is how an outer aterm
/// hands an inner one the identity its preminted edges already name. A test run
/// FROM INSIDE an aterm therefore has both variables in its own environment, and
/// every `aterm` it spawns adopts the SAME sid and the SAME launch nonce —
/// two nodes hosting one sid, a bare `@s-<sid>` that resolves to the sender
/// itself, and a "relaunch" whose successor has the identical epoch. Every one
/// of those is a silent pass or a silent fail depending on which way the
/// assertion points.
///
/// STILL REQUIRED, and by a narrower margin than it reads. `aterm`'s
/// `identity_claim` now lets only ONE live process answer to an adopted id, so
/// the second `aterm` a test spawns would mint its own — but the FIRST one
/// would still take the harness's own inherited identity, and a node wearing the
/// test runner's sid is the same two-hats failure. The variables are the problem
/// whatever the adopter does with them; strip them here.
/// DENY BY DEFAULT, DERIVED — not a list. This was eleven enumerated names, and
/// an enumerated boundary is the failure this repository keeps re-buying: the
/// list is complete on the day it is written and silently incomplete every day
/// after, because the thing that adds an `ATERM_*` variable is not the thing
/// that maintains this array. The same shape leaked `$HOME` past three `XDG_*`
/// redirects that read as hermetic (see [`scratch_home`]).
///
/// So the rule is now derived from the PARENT'S OWN ENVIRONMENT: every inherited
/// variable whose name begins with `ATERM_` is removed, whatever it is called
/// and whenever it was invented. The exceptions are NAMED rather than assumed —
/// the caller re-sets `ATERM_FABRIC_COMMAND` after this runs (the grid is the
/// `--lines`/`--columns` flags), and this sets [`STALE_HORIZON`] after the
/// removal. Automatic update checks and the package lane are switched off by
/// the fixture CONFIG ([`FIXTURE_GUI_CONFIG`]), the way a person switches them off,
/// and the reroute's stubs land under the scratch HOME. A variable
/// the test wants is one the test states; a variable it inherits is one nobody
/// chose.
///
/// **A deny-by-default that runs after its allow-list deletes the allow-list.**
/// `Command` applies env operations in order, so this must be called
/// immediately after `Command::new`, never at the end of the builder chain.
pub fn prepare_gui_environment(cmd: &mut Command) {
    for (name, _) in std::env::vars_os() {
        if name.to_string_lossy().starts_with("ATERM_") {
            cmd.env_remove(&name);
        }
    }
    cmd.env(STALE_HORIZON.0, STALE_HORIZON.1.as_millis().to_string());
}

/// Every launch's owed-answer horizon (`aterm-gui-fabric`'s
/// `ATERM_FABRIC_STALE_AFTER_MS` seam) at [`VERB_DEADLINE`], past every verb's
/// own wait, set by [`prepare_gui_environment`].
///
/// THE PRODUCTION SIX SECONDS IS A LATENCY BUDGET HERE, and no world's proof is
/// the stale verdict (`link_state` reads `stalled`; the endpoint's unit tests
/// age the owed clock and keep the default). A live bridge on a loaded machine
/// has owed an answer for six seconds while still moving it, and a `--wait`
/// post then ended `ERR fabric stale id=<n> queued=1` for a post that landed
/// moments later (`exactly_once`, 23 s under load against 2 s alone). A world
/// whose proof is the horizon names its own value in its environment, which a
/// launch applies after this. A build that reads no seams (an
/// `ATERM_TEST_EXECUTABLE` built without `dev-seams`) runs the production six
/// seconds, as it honours none of the suite's other seams (`ATERM_LINK_FAULT`,
/// the fast timers) either.
pub const STALE_HORIZON: (&str, Duration) = ("ATERM_FABRIC_STALE_AFTER_MS", VERB_DEADLINE);

/// Arm a `--headless` launch with a LIFELINE held for the life of this test
/// process (`aterm_uds::lifeline::arm_for_process`): the instance's `Drop`
/// still kills it on every ordinary exit, and when this process dies without
/// one — a SIGKILLed runner, a `timeout` — the kernel cuts the lifeline and the
/// instance shuts itself down instead of outliving the run (gap #36). Call after
/// `.stdin(..)`, before `spawn`. `dir` is the world's scratch root: the FIFO's
/// name lives there only until both ends are open.
///
/// The instance holds its stdio (the lifeline on stdin) and NOTHING ELSE of
/// this process (`inherit_nothing`): a std child on a std without the SP-1
/// strip keeps any descriptor another test thread had open unflagged at the
/// spawn — a `ps` or `lsof` pipe between its `pipe()` and its `F_SETFD` — and
/// an instance lives as long as its world, so that read would wait as long.
///
/// # Panics
/// The FIFO could not be made in `dir` — a broken fixture, never a reason to boot
/// an unwatched instance.
pub fn arm_lifeline(cmd: &mut Command, dir: &Path) {
    use aterm_uds::spawnfd::InheritNothing as _;
    cmd.inherit_nothing();
    aterm_uds::lifeline::arm_for_process(cmd, dir)
        .unwrap_or_else(|e| panic!("arm the instance's lifeline in {}: {e}", dir.display()));
}

/// These Fabric fixtures do not exercise update, package or machine configuration.
/// The app updater and automatic package updates are off (settings, 2026-09-23: no
/// environment veto exists), and so is automatic primer installation. Package opt-out
/// alone still runs machine settings, and a scratch HOME does not isolate macOS
/// preferences, so the machine policy is explicit.
pub const FIXTURE_GUI_CONFIG: &str = "agents_auto_prime = false\n\n[update]\nenabled = false\nauto_apply = false\n\n[packages]\nenabled = false\n\n[machine]\nuniversal_control = \"leave\"\n";

/// Seed a private config once; an intentional fixture edit survives relaunch.
pub fn prepare_fixture_config(tmp: &Path) {
    use std::io::Write;

    let dir = tmp.join("cfg/aterm");
    std::fs::create_dir_all(&dir).expect("scratch config dir");
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dir.join("aterm.toml"))
    {
        Ok(mut file) => file
            .write_all(FIXTURE_GUI_CONFIG.as_bytes())
            .expect("write isolated GUI config"),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => panic!("create isolated GUI config: {error}"),
    }
}

/// One booted world: a guarded broker, a headless aterm, and the bridge child
/// aterm launched. Torn down on every exit path, panics included.
/// The grants a node's cap file holds, as a function of its node id — what
/// [`World::boot_with_grants`] takes in place of the whole node ring.
pub type GrantsFor<'a> = &'a dyn Fn(&str) -> Vec<String>;

/// Exclusive scratch. An incomplete setup or teardown keeps its evidence.
struct Scratch {
    dir: Option<aterm_tempfile::TempDir>,
    keep: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl Drop for Scratch {
    fn drop(&mut self) {
        if let Some(dir) = self.dir.take() {
            eprintln!("KEPT fabric world {}", dir.keep().display());
        }
    }
}

/// Observe only: the child belongs to the instance, never this harness's reaper.
/// The parent and birth checks bind the exit descriptor to that instance's child.
fn child_exit(pid: i32, parent: u32) -> Result<std::os::fd::OwnedFd, String> {
    let birth = aterm_uds::process::read_process_birth(pid)
        .ok_or_else(|| format!("cannot read child {pid}'s birth"))?;
    if aterm_uds::process::read_process_parent(pid) != Some(parent) {
        return Err(format!(
            "{pid} is not an observed child of instance {parent}"
        ));
    }
    let fd = u32::try_from(pid)
        .ok()
        .and_then(aterm_uds::exitwatch::exit_fd)
        .ok_or_else(|| format!("cannot watch child {pid}'s exit"))?;
    if aterm_uds::process::read_process_birth(pid) != Some(birth)
        || aterm_uds::process::read_process_parent(pid) != Some(parent)
    {
        return Err(format!(
            "child {pid} changed across exit-watch registration"
        ));
    }
    Ok(fd)
}

fn exited(fd: &std::os::fd::OwnedFd) -> Result<bool, String> {
    use std::os::fd::AsRawFd as _;
    let mut p = libc::pollfd {
        fd: fd.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: one live owned exit descriptor and one writable pollfd; no wait.
    let n = unsafe { libc::poll(&mut p, 1, 0) };
    if n < 0 {
        let e = std::io::Error::last_os_error();
        return if e.kind() == std::io::ErrorKind::Interrupted {
            Ok(false)
        } else {
            Err(format!("poll child exit: {e}"))
        };
    }
    if p.revents & (libc::POLLERR | libc::POLLNVAL) != 0 {
        return Err(format!("invalid child exit watch: {}", p.revents));
    }
    Ok(p.revents & (libc::POLLIN | libc::POLLHUP) != 0)
}

/// A direct child is signalled only through its owned, cached-reap handle.
fn end_owned_child(child: &mut OwnedChild) -> Result<(), String> {
    let mut killed = false;
    for phase in 0..2 {
        let end = Instant::now() + DEADLINE;
        let mut exit = aterm_uds::deadline::ExitWait::new(child);
        loop {
            if let Some(status) = child
                .try_wait()
                .map_err(|e| format!("reap instance: {e}"))?
            {
                return if !killed && status.success() {
                    Ok(())
                } else {
                    Err(format!("instance did not finish a graceful quit: {status}"))
                };
            }
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            exit.wait(left);
        }
        if phase == 0 {
            child
                .kill()
                .map_err(|e| format!("kill owned instance: {e}"))?;
            killed = true;
        }
    }
    Err("owned instance not reaped after SIGKILL".into())
}

pub struct World {
    pub tmp: PathBuf,
    pub broker: Option<Broker>,
    pub handle: Option<BrokerHandle>,
    pub broker_sock: String,
    pub broker_log: String,
    pub gui: Option<OwnedChild>,
    pub ctl_sock: String,
    pub token: String,
    pub node: String,
    pub state: PathBuf,
    pub gui_log: PathBuf,
    /// How this world's own broker opens ([`broker_opts`]): Strict for every
    /// world but the ordered suite's and the fabric bench's weak arms
    /// ([`World::boot_opts`]). NOT the tier aterm opens its guarded broker at
    /// by default — ordered, at a 100 ms horizon, since the 2026-10-20 re-gate
    /// (`aterm_link::durability::default_for`) — so these worlds prove the
    /// bridge over strict acks. The ordered tier's own behaviour (the
    /// barrier, the escalation table's held head, the notifier's wait) is
    /// the ordered suite's — this directory's `ordered.rs` and `notify.rs`,
    /// whose worlds' brokers run at no horizon — and aterm-link's own
    /// notifier tests (`crates/aterm-link/src/notify.rs`), every one at no
    /// horizon but the one named next. The horizon's own flush is astream's
    /// `ordered_tier::the_horizon_flushes_an_idle_broker_on_its_timer` and
    /// `a_long_horizon_flushes_nothing_early` (`broker.ordered-tier`), and,
    /// at aterm's 100 ms, that aterm-link unit test,
    /// `notify::tests::in_follow_mode_the_horizon_alone_pages_an_attention_row`;
    /// the bench's default arm waits for it (`fabric_bench/run.rs`'s
    /// `await_durable`, a perf lane only). A restarted broker keeps it.
    pub opts: OpenOptions,
    /// Whether this world's broker retains its log by aterm's default rules
    /// ([`open_broker`]): every world's does but [`World::without_retention`]'s.
    /// A restarted broker keeps it.
    pub retained: bool,
    /// The read connection every polled broker read shares ([`World::read`]).
    watch: std::sync::Mutex<Option<Client>>,
    lifeline: std::sync::Mutex<Option<aterm_uds::lifeline::Lifeline>>,
    stopped: bool,
    scratch: Scratch,
}

impl Drop for World {
    fn drop(&mut self) {
        let ended = self.stop();
        self.watch
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(mut h) = self.handle.take() {
            h.shutdown();
        }
        self.broker.take();
        // `$ATERM_LINK_KEEP=1` leaves the scratch world behind, for the same
        // reason [`Fleet::drop`] honours it and with the same default: a failing
        // end-to-end test whose gui log, cap file and bridge state dir delete
        // themselves is a test nobody can diagnose. `Fleet` has had the hatch
        // since it was written; `World` deleted unconditionally — and most of
        // this crate's end-to-end suites are `World` suites, so for most of them
        // the evidence was unreachable.
        if ended.is_ok()
            && !std::thread::panicking()
            && !self.scratch.keep.load(std::sync::atomic::Ordering::Relaxed)
            && std::env::var_os("ATERM_LINK_KEEP").is_none()
            && let Some(scratch) = self.scratch.dir.take()
        {
            let path = scratch.keep();
            if let Err(e) = std::fs::remove_dir_all(&path) {
                eprintln!(
                    "KEPT {}: fabric scratch cleanup failed: {e}",
                    path.display()
                );
            }
        }
        if let Err(why) = ended {
            eprintln!(
                "KEPT {}: fabric process cleanup unknown: {why}",
                self.tmp.display()
            );
        }
    }
}

/// `kill(2)`, for the tests that need a real SIGKILL.
pub fn kill(pid: i32, sig: i32) {
    // SAFETY: `kill(2)` with a pid this test owns. No memory is touched.
    unsafe {
        libc_kill(pid, sig);
    }
}

unsafe extern "C" {
    #[link_name = "kill"]
    fn libc_kill(pid: i32, sig: i32) -> i32;
}

#[path = "../support/aterm_under_test.rs"]
mod aterm_under_test;

/// The binary every world runs: this package's own `aterm`, which cargo builds,
/// and rebuilds after any edit below it, before this suite runs — or the build
/// a private collector selected (`aterm_under_test`), the one every other launch
/// fixture runs then too. It is the window (`--headless`) and the bridge (`link
/// serve`) both.
pub fn aterm() -> &'static str {
    static ATERM: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    ATERM.get_or_init(|| {
        aterm_under_test::aterm_executable()
            .into_os_string()
            .into_string()
            .expect("the aterm under test at a UTF-8 path")
    })
}

/// [`aterm`] for a launch about to happen, run once first with `--version`
/// ([`aterm_spec::wait::first_exec`]): the system's first-exec wait on a
/// just-built binary (a minute, measured for a debug build) is paid here,
/// unclocked, and not inside [`DEADLINE`]. Call it right before the spawn:
/// cargo puts a new file at the binary's path on every invocation, so only the
/// file there now is the one launched.
pub fn aterm_to_launch() -> &'static str {
    let aterm = aterm();
    aterm_spec::wait::first_exec(Path::new(aterm), &["--version"]);
    aterm
}

impl World {
    /// Cut this world's lifeline, reap its direct instance, and observe the
    /// bridge and listed foreground roots exit. An exit descriptor proves
    /// termination, not this harness's reap of the instance's children. A
    /// failure preserves scratch; it never authorizes a signal by a pid file.
    ///
    /// # Errors
    /// A child identity/exit could not be observed, the instance needed a
    /// forced kill, or its owned handle could not confirm a successful reap.
    pub fn stop(&mut self) -> Result<(), String> {
        if self.stopped {
            return Ok(());
        }
        let mut failures = Vec::new();
        let mut watches = Vec::new();
        if let Some(gui) = &self.gui {
            let observed = (|| {
                let mut pids = Vec::new();
                let bridge = std::fs::read_to_string(self.state.join("pid"))
                    .map_err(|e| format!("read bridge pid: {e}"))?
                    .trim()
                    .parse::<i32>()
                    .map_err(|e| format!("parse bridge pid: {e}"))?;
                pids.push(bridge);
                let mut ctl = connect_within(&self.ctl_sock, &self.token, DEADLINE)
                    .map_err(|e| format!("connect for owned foreground roots: {e}"))?;
                let who = ctl
                    .request("who")
                    .map_err(|e| format!("read foreground roots: {e}"))?;
                if !who.ok() {
                    return Err(format!("foreground roots: {}", who.header()));
                }
                for row in who.rows() {
                    let pid = kv(row, "fgpgid")
                        .and_then(|p| p.parse::<i32>().ok())
                        .filter(|p| *p > 1)
                        .ok_or_else(|| format!("no foreground root in {row}"))?;
                    if !pids.contains(&pid) {
                        pids.push(pid);
                    }
                }
                for pid in pids {
                    watches.push((pid, child_exit(pid, gui.id())?));
                }
                Ok(())
            })();
            if let Err(why) = observed {
                failures.push(why);
            }
        } else {
            failures.push("no owned instance handle for child-exit observations".into());
        }
        self.lifeline
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(gui) = &mut self.gui {
            match end_owned_child(gui) {
                Ok(()) => {
                    self.gui = None;
                }
                Err(why) => failures.push(why),
            }
        }
        let end = Instant::now() + DEADLINE;
        while !watches.is_empty() {
            let mut pending = Vec::new();
            for (pid, fd) in watches {
                match exited(&fd) {
                    Ok(true) => {}
                    Ok(false) => pending.push((pid, fd)),
                    Err(why) => failures.push(format!("child {pid}: {why}")),
                }
            }
            watches = pending;
            if watches.is_empty() {
                break;
            }
            if Instant::now() >= end {
                failures.push(format!(
                    "child exits unconfirmed: {:?}",
                    watches.iter().map(|(pid, _)| pid).collect::<Vec<_>>()
                ));
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        if failures.is_empty() {
            self.stopped = true;
            Ok(())
        } else {
            Err(failures.join("; "))
        }
    }

    /// Boot everything. `tag` keeps parallel tests off each other's paths, and
    /// `accept_from` is the bridge's `--accept-from` list.
    pub fn boot(tag: &str, accept_from: &[&str]) -> Self {
        Self::boot_with(tag, accept_from, &[])
    }

    /// [`World::boot`] with extra environment for the bridge child (the fault
    /// injection the crash tests arm).
    pub fn boot_with(tag: &str, accept_from: &[&str], env: &[(&str, &str)]) -> Self {
        Self::boot_at(tag, accept_from, env, None)
    }

    /// [`World::boot_with`] with the bridge pointed at `broker_at` INSTEAD of a
    /// broker this harness serves — a path nothing listens on, or a stub the
    /// test serves itself. `broker` and `handle` are `None`; `broker_sock` is
    /// that path. Nothing here waits for `fabric=connected`, which such a
    /// world may never reach: that is what the `link_state` tests are about.
    pub fn boot_at(
        tag: &str,
        accept_from: &[&str],
        env: &[(&str, &str)],
        broker_at: Option<&str>,
    ) -> Self {
        Self::boot_full(
            tag,
            accept_from,
            env,
            broker_at,
            &[],
            None,
            broker_opts(aterm_link::durability::Durability::Strict),
        )
    }

    /// [`World::boot`] with extra flags on the bridge's `serve` line
    /// (`--presence minimal`, say).
    pub fn boot_flags(tag: &str, extra_serve_flags: &[&str]) -> Self {
        Self::boot_full(
            tag,
            &[],
            &[],
            None,
            extra_serve_flags,
            None,
            broker_opts(aterm_link::durability::Durability::Strict),
        )
    }

    /// [`World::boot_flags`] and [`World::boot_with`] at once: extra `serve`
    /// flags AND extra environment for the bridge child — a `--receipts`
    /// bridge under a fault the test arms.
    pub fn boot_flags_with(tag: &str, extra_serve_flags: &[&str], env: &[(&str, &str)]) -> Self {
        Self::boot_full(
            tag,
            &[],
            env,
            None,
            extra_serve_flags,
            None,
            broker_opts(aterm_link::durability::Durability::Strict),
        )
    }

    /// [`World::boot`] with this world's broker opened with `opts` instead of
    /// aterm's Strict options — the same guard ([`broker_opts`]), only what an
    /// ack promises changes. The ordered suite (`ordered.rs`) and the fabric
    /// bench's ordered (its default) and relaxed arms are its callers; every
    /// other test world stays Strict, which is not what aterm opens its
    /// guarded broker at by default (ordered: [`World::opts`]).
    pub fn boot_opts(tag: &str, opts: OpenOptions) -> Self {
        Self::boot_full(tag, &[], &[], None, &[], None, opts)
    }

    /// THE FABRIC BENCH'S ARM, whole: this world's broker opened with `opts`
    /// ([`World::boot_opts`]), RETAINED by aterm's default rules or not
    /// (`retained`: [`open_broker`] or [`open_broker_unretained`]), with extra
    /// `serve` flags for the bridge (`--log off`) and extra environment for
    /// the `aterm` instance, which the bridge child it launches inherits: the
    /// ping-pong arm's wait tally (`aterm_link::wait_tally::SEAM`, read by the
    /// bridge) and the handoff arm's quit seam (`ATERM_DEBUG_QUIT_BYE`, read
    /// by the instance, `aterm-gui` `fabric_launch.rs`). The bench measures
    /// what ships — retained, the bridge's `--log` left at `auto` — unless
    /// the arm names otherwise. [`World::without_retention`] is its strict,
    /// unretained case.
    pub fn boot_arm(
        tag: &str,
        opts: OpenOptions,
        retained: bool,
        extra_serve_flags: &[&str],
        env: &[(&str, &str)],
    ) -> Self {
        Self::boot_inner(tag, &[], env, None, extra_serve_flags, None, opts, retained)
    }

    /// [`World::boot`] with the node's cap file holding `grants(node)` instead
    /// of the whole node ring — a bridge whose cap is short of something.
    pub fn boot_with_grants(tag: &str, grants: GrantsFor<'_>) -> Self {
        Self::boot_full(
            tag,
            &[],
            &[],
            None,
            &[],
            Some(grants),
            broker_opts(aterm_link::durability::Durability::Strict),
        )
    }

    /// A world whose broker keeps NO retention rules: a broker from before
    /// astream's R4, or one `aterm link broker` serves when its rules cannot
    /// be set ([`open_broker_unretained`]), the named unretaining case (ruling
    /// U24). Restarted, it stays unretained.
    pub fn without_retention(tag: &str) -> Self {
        Self::boot_arm(
            tag,
            broker_opts(aterm_link::durability::Durability::Strict),
            false,
            &[],
            &[],
        )
    }

    fn boot_full(
        tag: &str,
        accept_from: &[&str],
        env: &[(&str, &str)],
        broker_at: Option<&str>,
        extra_serve_flags: &[&str],
        grants: Option<GrantsFor<'_>>,
        opts: OpenOptions,
    ) -> Self {
        Self::boot_inner(
            tag,
            accept_from,
            env,
            broker_at,
            extra_serve_flags,
            grants,
            opts,
            true,
        )
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "one private constructor every boot names"
    )]
    fn boot_inner(
        tag: &str,
        accept_from: &[&str],
        env: &[(&str, &str)],
        broker_at: Option<&str>,
        extra_serve_flags: &[&str],
        grants: Option<GrantsFor<'_>>,
        opts: OpenOptions,
        retained: bool,
    ) -> Self {
        // ONE PERMIT PER STACK COMING UP (see [`boot_permit`]). Dropped when this
        // function returns, which is after the control socket answers — so the
        // window it bounds is exactly the expensive one.
        let _boot = boot_permit();
        let scratch = Scratch {
            dir: Some(
                aterm_tempfile::Builder::new()
                    .prefix("atl-")
                    .tempdir_in("/tmp")
                    .expect("exclusive world scratch"),
            ),
            keep: Default::default(),
        };
        let tmp = scratch
            .dir
            .as_ref()
            .expect("new scratch")
            .path()
            .to_path_buf();
        std::fs::create_dir_all(tmp.join("run/aterm")).expect("scratch runtime dir");
        prepare_fixture_config(&tmp);
        let state = tmp.join("link");
        std::fs::create_dir_all(&state).expect("bridge state dir");

        // THE NODE ID IS PROVISIONED, not minted. A capability is bound to a
        // principal, so an operator mints the node's caps for an id the node
        // already has — which is exactly what seeding the state dir models.
        let node = format!("n-{:016x}", fnv(tag));
        std::fs::write(state.join("node"), format!("{node}\n")).expect("seed the node id");

        let broker_sock = broker_at.map_or_else(
            || tmp.join("b.sock").to_string_lossy().into_owned(),
            str::to_string,
        );
        let broker_log = tmp.join("b.log").to_string_lossy().into_owned();
        let ctl_sock = tmp
            .join("run/aterm/aterm.sock")
            .to_string_lossy()
            .into_owned();
        for p in [&broker_sock, &ctl_sock] {
            assert_sock_fits(p);
        }

        let (broker, handle) = if broker_at.is_some() {
            (None, None)
        } else {
            let broker = if retained {
                open_broker(&broker_log, &opts)
            } else {
                open_broker_unretained(&broker_log, &opts)
            };
            let handle = broker.serve(&broker_sock).expect("serve the broker");
            (Some(broker), Some(handle))
        };

        let cap_path = tmp.join("node.cap");
        let grants = grants.map_or_else(|| node_grants(&node), |f| f(&node));
        let caps: String = grants.iter().map(|g| cap_line(g)).collect();
        std::fs::write(&cap_path, caps).expect("write the node cap file");

        let mut fabric_cmd = format!(
            "{} link serve --fleet {FLEET} --broker {broker_sock} --cap-file {} --state {}",
            aterm_to_launch(),
            cap_path.display(),
            state.display()
        );
        if !accept_from.is_empty() {
            fabric_cmd.push_str(&format!(" --accept-from {}", accept_from.join(",")));
        }
        for flag in extra_serve_flags {
            fabric_cmd.push(' ');
            fabric_cmd.push_str(flag);
        }

        std::fs::write(tmp.join("fabric.cmd"), &fabric_cmd).expect("record the fabric command");
        let gui_log = tmp.join("gui.log");
        let out = std::fs::File::create(&gui_log).expect("gui log");
        let err = out.try_clone().expect("gui log clone");
        let mut cmd = aterm_uds::spawnfd::command(aterm_to_launch());
        // BEFORE the explicit `.env` calls below, never after: `Command`
        // applies env operations in order, so a blanket removal that ran last
        // would delete the three variables this harness deliberately sets.
        scratch_spawnfd(&mut cmd, &tmp);
        cmd.arg("--headless")
            .env("HOME", scratch_home(&tmp))
            // Every launch's TMPDIR is its scratch: a FIFO a SIGKILLed spawn left goes with it.
            .env("TMPDIR", &tmp)
            .env("XDG_RUNTIME_DIR", tmp.join("run"))
            .env("XDG_CONFIG_HOME", tmp.join("cfg"))
            .env("SHELL", "/bin/sh")
            .args(["--lines", "40", "--columns", "120"])
            .env("ATERM_FABRIC_COMMAND", &fabric_cmd)
            .stdin(OwnedStdio::null())
            .stdout(out)
            .stderr(err);
        for (k, v) in env {
            cmd.env(k, v);
        }
        let lifeline =
            aterm_uds::lifeline::Lifeline::arm(&mut cmd, &tmp).expect("the world's lifeline");

        let mut world = World {
            tmp,
            broker,
            handle,
            broker_sock,
            broker_log,
            gui: None,
            ctl_sock,
            token: String::new(),
            node,
            state,
            gui_log,
            opts,
            retained,
            watch: std::sync::Mutex::new(None),
            lifeline: std::sync::Mutex::new(Some(lifeline)),
            stopped: false,
            scratch,
        };
        world.gui = Some(cmd.spawn().expect("launch aterm --headless"));
        let gui = world.gui.as_mut().expect("the gui just spawned");
        world.token = World::wait_for_owned_token_of(&world.ctl_sock, gui, &world.gui_log);
        world
    }

    /// A fresh authenticated control connection.
    pub fn ctl(&self) -> Ctl {
        connect(&self.ctl_sock, &self.token).expect("connect to aterm")
    }

    /// Run one verb and return the reply.
    pub fn verb(&self, line: &str) -> Reply {
        self.ctl()
            .request(line)
            .unwrap_or_else(|e| panic!("{line}: {e}"))
    }

    /// A god-cap broker client for the test itself: the fleet root, which may
    /// publish under any producer id. That is what lets a test forge the records
    /// the bridge must refuse. For READING the bus, [`World::read`] and
    /// [`World::row`]: a fresh god client per poll is the load they exist to
    /// remove.
    pub fn god(&self) -> Client {
        let mut c = broker_client(&self.broker_sock);
        let cap = astream_cap::mint(SECRET, &format!("/f/{FLEET}/>")).expect("mint the god cap");
        c.attach(&cap.filter, &cap.tag).expect("attach the god cap");
        c
    }

    /// Run one broker READ (`last`, `fetch`) on this world's shared read
    /// connection — see [`read_on`] for why one is shared and what happens when
    /// a broker restart has closed it.
    ///
    /// # Errors
    ///
    /// The connect, the attach, or `f` on a connection opened for it.
    pub fn read<T>(&self, f: impl FnMut(&mut Client) -> std::io::Result<T>) -> std::io::Result<T> {
        read_on(
            &self.watch,
            || {
                let s = std::os::unix::net::UnixStream::connect(&self.broker_sock)?;
                s.set_read_timeout(Some(VERB_DEADLINE))?;
                s.set_write_timeout(Some(VERB_DEADLINE))?;
                let mut c = Client::from_stream(s);
                let cap = read_cap();
                c.attach(&cap.filter, &cap.tag)?;
                Ok(c)
            },
            f,
        )
    }

    /// The body of the retained row at exactly `subject`, or `None` when there
    /// is none (or the read failed), over [`World::read`]'s connection.
    pub fn row(&self, subject: &str) -> Option<String> {
        let page = self.read(|c| c.last(subject, "", 8)).ok()?;
        exact_body(page, subject)
    }

    /// Wait until the bridge is attached and its presence row says `live`.
    pub fn wait_ready(&self) {
        until("aterm to report fabric=connected", || {
            self.verb("status")
                .header()
                .contains("fabric=connected")
                .then_some(())
        });
        let subject = format!("/f/{FLEET}/pub/{}/node/presence", self.node);
        until("the node's presence row to say live", || {
            self.row(&subject)
                .filter(|b| b.contains("state=live"))
                .map(|_| ())
        });
    }

    /// TAKE THE BROKER AWAY, and wait until the bridge has noticed.
    ///
    /// `shutdown` force-closes every connection, which is what a `kill -9` of a
    /// broker process does to its peers; the broker and its socket file go
    /// with it, and the log stays, so [`World::start_broker`] brings the same
    /// records back.
    ///
    /// THE WAIT IS THE POINT. The bridge learns of a force-closed connection
    /// only from its reader's EOF, several thread hops after `shutdown`
    /// returns, so a status read before it can still see the last `connected`.
    /// A test that then restarts the broker and waits for `connected` is
    /// satisfied by the state from BEFORE the outage, and the late `link down`
    /// refuses its next `--wait` post with `ERR fabric stalled`; a test that
    /// samples a property "during the outage" samples it before the bridge's
    /// link-loss path has run at all. So this returns only once `fabric status`
    /// reads `reason=no-socket`: that is the REDIAL's verdict on the missing
    /// socket file, so the bridge has run its whole link-loss path and gone back
    /// to dialing, and it stays there until a broker is served at this path
    /// again — a state that cannot be missed by polling.
    pub fn stop_broker(&mut self) {
        if let Some(mut h) = self.handle.take() {
            h.shutdown();
        }
        self.broker.take();
        let _ = std::fs::remove_file(&self.broker_sock);
        until("the bridge to notice the broker gone and redial it", || {
            let fs = self.verb("fabric status").header().to_string();
            (kv(&fs, "state") == Some("stalled") && kv(&fs, "reason") == Some("no-socket"))
                .then_some(())
        });
    }

    /// BRING THE BROKER BACK on the same log and socket path, and wait for the
    /// bridge to reattach. The case R7 is about: the broker rebuilds its dedup
    /// map from the log it kept, and fires every will that log holds at open.
    /// After [`World::stop_broker`] saw the link down, the `connected` this
    /// waits for can only be the new attach's.
    pub fn start_broker(&mut self) {
        let _ = std::fs::remove_file(&self.broker_sock);
        let broker = if self.retained {
            open_broker(&self.broker_log, &self.opts)
        } else {
            open_broker_unretained(&self.broker_log, &self.opts)
        };
        let handle = broker
            .serve(&self.broker_sock)
            .expect("serve the broker again");
        self.broker = Some(broker);
        self.handle = Some(handle);
        until("the bridge to reattach to the restarted broker", || {
            self.verb("status")
                .header()
                .contains(" fabric=connected ")
                .then_some(())
        });
    }

    /// `SIGKILL` the bridge, and wait for aterm's supervisor to bring a
    /// replacement up, attached. The NEW PID comes first, and that order is
    /// what makes the `connected` after it the replacement's: a bridge writes
    /// its pid only once its aterm connection is up, and aterm resets the link
    /// to `stalled` when a new bridge attaches to it.
    pub fn restart_bridge(&self) {
        let pid = self.bridge_pid();
        kill(pid, 9);
        until("the bridge to come back under a new pid", || {
            let back: i32 = std::fs::read_to_string(self.state.join("pid"))
                .ok()?
                .trim()
                .parse()
                .ok()?;
            (back != pid).then_some(())
        });
        until("the replacement bridge to attach", || {
            self.verb("status")
                .header()
                .contains(" fabric=connected ")
                .then_some(())
        });
    }

    /// AN ORDERING BARRIER for "the bridge published nothing more on the
    /// endpoint's behalf": `sid` posts a note to itself and waits for it to
    /// land. A negative count read after it is read after every drain that
    /// could have published what it says is absent — no sleep, and no window a
    /// late publish can land behind.
    ///
    /// Why the landing orders it: the post is queued after every earlier verb
    /// has returned, so the drain that publishes it peeked the outbox after
    /// them; that peek lists EVERY session's owed receipts (aterm's
    /// `cmd_outbox`, outside its post budget), and a drain publishes (or, under
    /// `--no-receipts`, retires) the receipts it lists before it publishes any
    /// post (`Bridge::drain_outbox`). So by the time this post has landed, a
    /// receipt any earlier verb owed is on the bus already, and one retired was
    /// retired first. (A receipt whose publish FAILED stays owed for a later
    /// drain: a broker refusal, which no count after this is about.)
    pub fn drained(&self, sid: &str) {
        let r = self.verb(&format!(
            "@{sid} post to=@{sid} kind=note --wait=60000 drain-barrier"
        ));
        assert!(
            r.ok(),
            "the drain barrier: {} — log:\n{}",
            r.header(),
            self.log_tail()
        );
    }

    /// Every session aterm hosts, as `(local, sid, nonce)`.
    pub fn sessions(&self) -> Vec<(u64, String, String)> {
        session_rows(&self.verb("sessions"))
    }

    /// The boot session's sid, and one more spawned beside it. A3 is a ONE-NODE
    /// rung, so both peers are sessions of this instance.
    pub fn two_sessions(&self) -> (String, String) {
        let first = until("aterm's boot session", || {
            self.sessions().first().map(|(_, sid, _)| sid.clone())
        });
        let reply = self.verb("spawn");
        assert!(reply.ok(), "spawn: {}", reply.header());
        let second = reply
            .header()
            .split_whitespace()
            .nth(1)
            .expect("spawn answers OK <sid>")
            .to_string();
        until("the bridge to see both sessions", || {
            (self.sessions().len() >= 2).then_some(())
        });
        (first, second)
    }

    /// A SECOND headless instance in this world's socket dir, with no fabric
    /// command and so no bridge of its own — what a node with two aterm windows
    /// looks like to the first one's bridge. The world is first re-pointed at
    /// its own instance's per-pid socket, because the newcomer takes the
    /// `aterm.sock` latest alias.
    pub fn sibling_instance(&mut self) -> Sibling {
        self.sibling_with(None)
    }

    /// [`World::sibling_instance`] ARMED with this world's own fabric command,
    /// `--state` and all — what every later aterm launch on a machine where
    /// `fabric on` wrote `[fabric] command` looks like.
    pub fn sibling_instance_armed(&mut self) -> Sibling {
        let fabric = std::fs::read_to_string(self.tmp.join("fabric.cmd"))
            .expect("this world's fabric command");
        self.sibling_with(Some(&fabric))
    }

    fn sibling_with(&mut self, fabric: Option<&str>) -> Sibling {
        // The independently owned sibling may outlive this World.
        self.scratch
            .keep
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let own = self.gui.as_ref().expect("this world's gui").id();
        self.ctl_sock = self
            .tmp
            .join(format!("run/aterm/aterm-{own}.sock"))
            .to_string_lossy()
            .into_owned();
        self.token = World::wait_for_token_at(&self.ctl_sock);
        let log = std::fs::File::create(self.tmp.join("sibling.log")).expect("sibling log");
        let err = log.try_clone().expect("sibling log clone");
        let mut cmd = Command::new(aterm_to_launch());
        prepare_gui_environment(&mut cmd);
        cmd.arg("--headless")
            .env("HOME", scratch_home(&self.tmp))
            .env("TMPDIR", &self.tmp)
            .env("XDG_RUNTIME_DIR", self.tmp.join("run"))
            .env("XDG_CONFIG_HOME", self.tmp.join("cfg"))
            .env("SHELL", "/bin/sh")
            .args(["--lines", "40", "--columns", "120"])
            .stdin(Stdio::null())
            .stdout(log)
            .stderr(err);
        if let Some(fabric) = fabric {
            cmd.env("ATERM_FABRIC_COMMAND", fabric);
        }
        arm_lifeline(&mut cmd, &self.tmp);
        let mut child = cmd.spawn().expect("launch a sibling aterm --headless");
        let sock = self
            .tmp
            .join(format!("run/aterm/aterm-{}.sock", child.id()))
            .to_string_lossy()
            .into_owned();
        let token = World::wait_for_token_of(&sock, &mut child, &self.tmp.join("sibling.log"));
        Sibling { child, sock, token }
    }

    /// A SUCCESSOR INSTANCE on this world's node: one more headless `aterm`
    /// ARMED with this world's own fabric command — the same broker, cap file
    /// and `--state`, so its bridge is a second bridge on the one state dir —
    /// in a runtime dir of its own (`run-<n>`), so its control socket is its
    /// own and the world's stays where it is. Its bridge finds the dir held
    /// and STANDS BY (`link down reason=state-locked`, trying the lock every
    /// second) until the holder exits, then carries the node: the bridges'
    /// half of a seamless update's successor, which a headless instance
    /// cannot reach by updating. `env` reaches the instance and its bridge.
    pub fn successor_instance(&self, n: usize, env: &[(&str, &str)]) -> Successor {
        // A successor can outlive this World. Its shared paths must survive
        // independently of which owner drops first, even before readiness.
        self.scratch
            .keep
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let fabric = std::fs::read_to_string(self.tmp.join("fabric.cmd"))
            .expect("this world's fabric command");
        let run = self.tmp.join(format!("run-{n}"));
        std::fs::create_dir_all(run.join("aterm")).expect("the successor's runtime dir");
        let sock = run.join("aterm/aterm.sock").to_string_lossy().into_owned();
        assert_sock_fits(&sock);
        let log_path = self.tmp.join(format!("successor-{n}.log"));
        let log = std::fs::File::create(&log_path).expect("successor log");
        let err = log.try_clone().expect("successor log clone");
        let mut cmd = aterm_uds::spawnfd::command(aterm_to_launch());
        scratch_spawnfd(&mut cmd, &self.tmp);
        cmd.arg("--headless")
            .env("HOME", scratch_home(&self.tmp))
            .env("TMPDIR", &self.tmp)
            .env("XDG_RUNTIME_DIR", &run)
            .env("XDG_CONFIG_HOME", self.tmp.join("cfg"))
            .env("SHELL", "/bin/sh")
            .args(["--lines", "40", "--columns", "120"])
            .env("ATERM_FABRIC_COMMAND", fabric.trim())
            .stdin(OwnedStdio::null())
            .stdout(log)
            .stderr(err);
        for (k, v) in env {
            cmd.env(k, v);
        }
        let lifeline =
            aterm_uds::lifeline::Lifeline::arm(&mut cmd, &self.tmp).expect("successor lifeline");
        let mut successor = Successor {
            child: None,
            lifeline: Some(lifeline),
            sock,
            token: String::new(),
            log: log_path,
        };
        successor.child = Some(cmd.spawn().expect("launch a successor aterm --headless"));
        successor.token = World::wait_for_owned_token_of(
            &successor.sock,
            successor.child.as_mut().expect("spawned successor"),
            &successor.log,
        );
        successor
    }

    /// This session's `inbox` rows (`--peek`, so the watermarks do not move).
    pub fn inbox(&self, sid: &str) -> Vec<String> {
        inbox_rows(&self.verb(&format!("@{sid} inbox --peek")))
    }

    /// `@{from} post to=@{to} kind={kind} {flags} --wait=60000 {text}`, answered with its
    /// header and the offset it LANDED at. The wait is a minute's hang detector, and no
    /// loaded bridge cuts it short at the wedged-helper horizon: every world sets that
    /// horizon past it ([`STALE_HORIZON`]). Any refusal fails here with the header and the
    /// log.
    pub fn post_landed(
        &self,
        from: &str,
        to: &str,
        kind: &str,
        flags: &str,
        text: &str,
    ) -> (String, u64) {
        let reply = self.verb(&format!(
            "@{from} post to=@{to} kind={kind} {flags} --wait=60000 {text}"
        ));
        let header = reply.header().to_string();
        assert!(
            reply.ok(),
            "@{from}'s post to @{to}: {header} — log:\n{}",
            self.log_tail()
        );
        let off = off_of(&header);
        (header, off)
    }

    /// The bridge child's pid, from its state dir.
    pub fn bridge_pid(&self) -> i32 {
        until("the bridge to write its pid", || {
            std::fs::read_to_string(self.state.join("pid"))
                .ok()?
                .trim()
                .parse()
                .ok()
        })
    }

    /// Every `ev` record the node has published, newest last — over EVERY `ev`
    /// face it owns.
    ///
    /// §3.3 makes `ev` a per-owner face and §10 puts a session's own verdicts on
    /// the SESSION's one (`/f/<F>/pub/<node>/<sid>/ev`), so a reader that looked
    /// only at `…/node/ev` would miss every `applied`, `refused` and
    /// `undeliverable` the fabric publishes about a session — which is most of
    /// them. The whole `pub/<node>/` subtree is read and the `ev` leaves kept.
    pub fn ev(&self) -> Vec<String> {
        self.ev_on(&format!("/f/{FLEET}/pub/{}/>", self.node))
    }

    /// Every `ev` record under one face or filter, newest last — a single
    /// owner's (`subject::session_face`, `subject::node_face`) where
    /// [`World::ev`] reads them all.
    pub fn ev_on(&self, filter: &str) -> Vec<String> {
        self.read(|c| ev_rows(c, filter))
            .unwrap_or_else(|e| panic!("read the ev records under {filter}: {e}"))
    }

    /// The tail of aterm's own log, for a failure message.
    pub fn log_tail(&self) -> String {
        log_tail_of(&self.gui_log)
    }
}

/// The capability [`World::read`] and [`Fleet::read`] attach: READ ONLY over
/// the whole test fleet. Every read a test makes is contained in it, and a
/// read grant binds no principal, so the broker has no unbound read-write
/// grant to warn about on every connection (`astream-broker`'s
/// `attach_grant`) — the god cap's one stderr line per attach.
fn read_cap() -> astream_cap::Capability {
    astream_cap::mint(SECRET, &format!("ro:/f/{FLEET}/>")).expect("mint the read cap")
}

/// Run `f` — one broker READ — on the connection `slot` holds, opening one
/// with `open` when it holds none. The core of [`World::read`] and
/// [`Fleet::read`].
///
/// WHY ONE CONNECTION. Every `until` polls every 10 ms, and a reader that
/// opened its own connection per call made each poll a connect, a `Hello`, an
/// HMAC attach the broker verifies, a broker thread and (under the god cap) a
/// warning line — up to six thousand of them for one wait the machine was
/// merely slow to satisfy, competing with the bridge for the broker the wait
/// was about (the two-node fleet measured its waits timing out on it).
///
/// WHY IT HEALS. A broker restart force-closes the connection
/// (`BrokerHandle::shutdown`), so a REUSED connection that fails is dropped
/// and `f` runs once on a fresh one — which is exactly what every read did
/// before the connection was shared, so the sharing changes no answer. A
/// failure on a FRESH connection is the answer and is returned: nothing here
/// retries a read that failed on its own terms. Reads only: `f` may run twice,
/// which a publish must never do.
fn read_on<S: std::io::Read + std::io::Write, T>(
    slot: &std::sync::Mutex<Option<astream_broker::Client<S>>>,
    open: impl FnOnce() -> std::io::Result<astream_broker::Client<S>>,
    mut f: impl FnMut(&mut astream_broker::Client<S>) -> std::io::Result<T>,
) -> std::io::Result<T> {
    let mut held = slot
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(c) = held.as_mut() {
        if let Ok(v) = f(c) {
            return Ok(v);
        }
        *held = None;
    }
    let mut c = open()?;
    let v = f(&mut c)?;
    *held = Some(c);
    Ok(v)
}

/// The body of the row a `last` page holds at exactly `subject`.
fn exact_body(page: astream_broker::Page, subject: &str) -> Option<String> {
    page.0
        .into_iter()
        .find(|(_, s, _)| s == subject)
        .map(|(_, _, b)| String::from_utf8_lossy(&b).into_owned())
}

/// Every `ev` payload under `filter`, oldest first. Shared by [`World::ev`] and
/// [`Node::ev`] so the two can never read different faces.
///
/// # Errors
///
/// A `Fetch` that failed part-way: never a partial answer read as the whole.
fn ev_rows<S: std::io::Read + std::io::Write>(
    conn: &mut astream_broker::Client<S>,
    filter: &str,
) -> std::io::Result<Vec<String>> {
    let mut out = Vec::new();
    let mut from = 0u64;
    loop {
        let (rows, (next, head)) = conn.fetch(from, filter, 256)?;
        for (_, subject, body) in rows {
            if !subject.ends_with("/ev") {
                continue;
            }
            let (b, _) = aterm_link::body::Body::decode(&body);
            if let Some(ev) = b.unknown.get("ev") {
                out.push(aterm_link::pct::decode(ev));
            }
        }
        if next >= head || next <= from {
            return Ok(out);
        }
        from = next;
    }
}

/// A `sessions` reply as `(local, sid, nonce)` rows. Shared by
/// [`World::sessions`] and [`Node::sessions`].
fn session_rows(reply: &Reply) -> Vec<(u64, String, String)> {
    reply
        .rows()
        .iter()
        .filter_map(|row| {
            let f: Vec<&str> = row.split_whitespace().collect();
            let local = f.first()?.parse().ok()?;
            let sid = (*f.get(1)?).to_string();
            let nonce = f.iter().find_map(|t| t.strip_prefix("nonce="))?.to_string();
            Some((local, sid, nonce))
        })
        .collect()
}

/// The `msg` rows of an `inbox --peek` reply. Shared by [`World::inbox`] and
/// [`Node::inbox`].
fn inbox_rows(reply: &Reply) -> Vec<String> {
    reply
        .rows()
        .iter()
        .filter(|r| r.starts_with("msg "))
        .cloned()
        .collect()
}

/// The last 20 lines of an aterm log, for a failure message.
fn log_tail_of(path: &Path) -> String {
    let body = std::fs::read_to_string(path).unwrap_or_default();
    let lines: Vec<&str> = body.lines().collect();
    lines[lines.len().saturating_sub(20)..].join("\n")
}

/// `key=` of a whitespace-token line.
pub use aterm_link::fabric::kv;

/// The offset an `OK <id> off=<n> …` header names.
pub fn off_of(header: &str) -> u64 {
    kv(header, "off")
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("a landing offset in: {header}"))
}

/// The row id of `sid`'s `inbox` row at broker offset `off`, `--peek` so nothing
/// is marked — the row a recipient is about to acknowledge.
pub fn id_at_off(w: &World, sid: &str, off: u64) -> Option<u64> {
    w.inbox(sid).into_iter().find_map(|r| {
        (kv(&r, "off") == Some(&off.to_string()))
            .then(|| r.split_whitespace().nth(1)?.parse().ok())
            .flatten()
    })
}

/// One record for [`publish_pipelined`]: `(producer_id, producer_seq, subject,
/// body)`, exactly the arguments of `Client::publish`.
pub type Record = (u64, u64, String, Vec<u8>);

/// Publish `records` in order on `conn`, as a loop of `conn.publish` would, and
/// answer each one's `(offset, deduped)` in that order — with the acks read
/// BEHIND the sends (astream's pipelined publish), never one round trip each.
///
/// WHY. Every test world's broker but the ordered suite's is Strict
/// ([`broker_opts`], [`World::opts`]):
/// an ack is an fsync, and a test that seeds a backlog one blocking `publish`
/// at a time pays one fsync per record: `x2`'s 560 fillers took 5.9 s of its
/// 7.2 s alone (10.5 ms a record), and the world suite seeds 2,806 records that
/// way — ~30 s of live-stack time spent waiting on the disk, more beside a
/// concurrent compile's I/O. In flight together the publishes share
/// group-commit fsyncs (`Client::send_publish`'s contract): the same 560 land
/// in 77 ms.
///
/// THE SAME RECORDS IN THE SAME ORDER. One connection, requests sent in
/// `records` order, and the broker appends and acks a connection's requests in
/// the order they arrive: every record gets the offset the serial loop would
/// have given it relative to the others, under the same `(producer, seq)` key.
/// What changes is only how fast they land relative to the bridge reading
/// them — which the serial loop never fixed either.
///
/// The window stays well under `Client::MAX_PIPELINE_WINDOW`, so the
/// interleaved send/receive cannot wedge the connection.
///
/// # Panics
///
/// A record the broker refused or a transport failure, naming the record's
/// index and subject — as the loop's `expect` did.
pub fn publish_pipelined(conn: &mut Client, records: &[Record]) -> Vec<(u64, bool)> {
    const WINDOW: usize = 128;
    let mut acks = Vec::with_capacity(records.len());
    let mut sent = 0;
    while acks.len() < records.len() {
        while sent < records.len() && sent - acks.len() < WINDOW {
            let (producer, seq, subject, body) = &records[sent];
            conn.send_publish(*producer, *seq, subject, body)
                .unwrap_or_else(|e| panic!("publish record {sent} on {subject}: {e}"));
            sent += 1;
        }
        let i = acks.len();
        let ack = conn
            .recv_publish_ack()
            .unwrap_or_else(|e| panic!("publish record {i} on {}: {e}", records[i].2));
        acks.push(ack);
    }
    acks
}

/// Every record currently on the fleet's broadcast subtree.
pub fn say_records(w: &World) -> Vec<(u64, String, String)> {
    let filter = format!("/f/{FLEET}/pub/*/*/say/>");
    w.read(|c| {
        let mut out = Vec::new();
        let mut from = 0u64;
        loop {
            let (rows, (next, head)) = c.fetch(from, &filter, 256)?;
            for (off, subject, body) in rows {
                out.push((off, subject, String::from_utf8_lossy(&body).into_owned()));
            }
            if next >= head {
                return Ok(out);
            }
            from = next.max(from + 1);
        }
    })
    .expect("fetch the say subtree")
}

/// The rows of a session's ring that arrived on `topic`.
pub fn topic_rows(w: &World, sid: &str, topic: &str) -> Vec<String> {
    w.inbox(sid)
        .into_iter()
        .filter(|r| r.contains(&format!(" topic={topic} ")))
        .collect()
}

/// `Some` once the BRIDGE holds `topic` for `sid`: its persisted cursors, one
/// topic per line.
pub fn bridge_holds(w: &World, sid: &str, topic: &str) -> Option<()> {
    std::fs::read_to_string(w.state.join(format!("topics/{sid}")))
        .ok()?
        .lines()
        .any(|l| l.split(' ').next() == Some(topic))
        .then_some(())
}

/// How many push-lane watches aterm counts on `sid` — `who`'s `watchers=` —
/// or `None` while `who` does not answer it, so a wait polls on. aterm
/// registers a watch only after seeding it at the session's timeline high, and
/// queues its `sub` ack in the same wake, ahead of every frame the watch
/// drains (aterm-gui `subscribe.rs`). So `>= 1` says every record made from
/// now on is above the watch's watermark, and the ack is ordered before every
/// frame of any later wake.
pub fn watchers(w: &World, sid: &str) -> Option<u64> {
    let reply = w.verb("who");
    if !reply.ok() {
        return None;
    }
    reply
        .rows()
        .iter()
        .find(|row| row.split_whitespace().nth(1) == Some(sid))?
        .split_whitespace()
        .find_map(|t| t.strip_prefix("watchers="))?
        .parse()
        .ok()
}

/// The endpoint's debug-only PUSH HOLDS, armed by `$ATERM_TEST_PUSH_HOLD`
/// (aterm-gui `subscribe.rs`, `push_held`). While a file named for a step
/// stands in the directory, the push loop skips that step and keeps its
/// cursor, and the first wake after [`Hold::off`] resumes from it. The steps:
/// `adopt` (watch a session the store lists), `sessions` (announce it on the
/// roster) and `timeline` (drain a watch's timeline — read under the lock
/// each record is appended under, so a record made after [`Hold::on`] returns
/// is withheld until [`Hold::off`], whatever the push loop was doing, except a
/// closing session's final pass, and what the ring evicted meanwhile comes
/// back as a `GAP`). A held membership state is one aterm reaches by itself;
/// the hold chooses only how long it lasts. The `timeline` hold withholds that
/// stream alone, so a test may rely on the timeline's own order and the `GAP`
/// count, not on order across streams.
pub struct Hold(aterm_tempfile::TempDir);

impl Hold {
    /// An exclusively created hold directory for the test `tag`.
    pub fn new(tag: &str) -> Self {
        let dir = aterm_tempfile::Builder::new()
            .prefix(&format!("atl-{tag}-hold-"))
            .tempdir()
            .expect("exclusive hold directory");
        Hold(dir)
    }

    /// The value of `$ATERM_TEST_PUSH_HOLD` that arms it in aterm.
    pub fn env(&self) -> String {
        self.0.path().to_string_lossy().into_owned()
    }

    /// Hold `step`.
    pub fn on(&self, step: &str) {
        std::fs::write(self.0.path().join(step), b"")
            .unwrap_or_else(|e| panic!("hold {step}: {e}"));
    }

    /// Release `step`.
    pub fn off(&self, step: &str) {
        std::fs::remove_file(self.0.path().join(step))
            .unwrap_or_else(|e| panic!("release {step}: {e}"));
    }
}

/// The session's presence row on the bus, as its body.
pub fn presence_body(w: &World, sid: &str) -> Option<String> {
    w.row(&format!("/f/{FLEET}/pub/{}/{sid}/presence", w.node))
}

/// A file's permission bits.
pub fn mode_of(p: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).expect("metadata").permissions().mode() & 0o777
}

/// A tiny FNV-1a over the test tag, so each test's node id is distinct and
/// stable — distinct because the tests run in one process against one `/tmp`,
/// stable because a capability is bound to the id.
fn fnv(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x1000_0000_01b3);
    }
    h
}

/// Whether a pid is still alive (`kill(pid, 0)`).
#[must_use]
pub fn alive(pid: i32) -> bool {
    // SAFETY: signal 0 is the liveness probe; it delivers nothing and touches no
    // memory.
    unsafe { libc_kill(pid, 0) == 0 }
}

impl World {
    /// Relaunch `aterm` against the same scratch world — the same broker,
    /// the same bridge state dir, the same node id, the same fabric command.
    pub fn relaunch_gui(w: &World) -> OwnedChild {
        let fabric_cmd = std::fs::read_to_string(w.tmp.join("fabric.cmd"))
            .expect("the boot recorded its fabric command");
        let out = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(&w.gui_log)
            .expect("gui log");
        let err = out.try_clone().expect("gui log clone");
        let mut cmd = aterm_uds::spawnfd::command(aterm_to_launch());
        // BEFORE the explicit `.env` calls below, never after: `Command`
        // applies env operations in order, so a blanket removal that ran last
        // would delete the three variables this harness deliberately sets.
        scratch_spawnfd(&mut cmd, &w.tmp);
        cmd.arg("--headless")
            .env("HOME", scratch_home(&w.tmp))
            .env("TMPDIR", &w.tmp)
            .env("XDG_RUNTIME_DIR", w.tmp.join("run"))
            .env("XDG_CONFIG_HOME", w.tmp.join("cfg"))
            .env("SHELL", "/bin/sh")
            .args(["--lines", "40", "--columns", "120"])
            .env("ATERM_FABRIC_COMMAND", fabric_cmd.trim())
            .stdin(OwnedStdio::null())
            .stdout(out)
            .stderr(err);
        let lifeline =
            aterm_uds::lifeline::Lifeline::arm(&mut cmd, &w.tmp).expect("relaunch lifeline");
        *w.lifeline
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(lifeline);
        cmd.spawn().expect("relaunch aterm --headless")
    }

    /// The instance token for a control socket that is coming up.
    ///
    /// The token FILE existing is not enough — a dead instance leaves one
    /// behind, and the `latest` alias repoints only when the successor binds. So
    /// the wait ends on a connection that ANSWERS, which is the observable state
    /// the test actually depends on.
    ///
    pub fn wait_for_token_at(sock: &str) -> String {
        Self::wait_for_token(sock, || {})
    }

    /// [`World::wait_for_token_at`] for the instance this test just spawned as
    /// `child`, logging to `log`: a child that EXITS before it answers fails the
    /// wait at once, naming its exit status and its log's tail, rather than
    /// leaving the test to poll a socket nobody will ever bind until the
    /// deadline. A wait that FAILS kills and reaps `child` before it re-raises,
    /// so an instance alive but never answering does not outlive its test.
    pub fn wait_for_token_of(sock: &str, child: &mut Child, log: &Path) -> String {
        let waited = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            Self::wait_for_token(sock, || {
                if let Ok(Some(exit)) = child.try_wait() {
                    panic!(
                        "aterm exited ({exit}) before it answered on {sock}; its log:\n{}",
                        log_tail_of(log)
                    );
                }
            })
        }));
        waited.unwrap_or_else(|why| {
            let _ = child.kill();
            let _ = child.wait();
            std::panic::resume_unwind(why)
        })
    }

    pub fn wait_for_owned_token_of(sock: &str, child: &mut OwnedChild, log: &Path) -> String {
        let waited = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            Self::wait_for_token(sock, || {
                if let Ok(Some(exit)) = child.try_wait() {
                    panic!(
                        "aterm exited ({exit}) before it answered on {sock}; its log:\n{}",
                        log_tail_of(log)
                    );
                }
            })
        }));
        waited.unwrap_or_else(|why| {
            let _ = child.kill();
            if let Err(cleanup) = end_owned_child(child) {
                eprintln!("instance readiness cleanup: {cleanup}");
            }
            std::panic::resume_unwind(why)
        })
    }

    fn wait_for_token(sock: &str, mut check_child: impl FnMut()) -> String {
        until("aterm to bind and answer on its control socket", || {
            check_child();
            let path = aterm_uds::latest::token_path_for_sock(sock)?;
            let token = std::fs::read_to_string(path).ok()?;
            let token = token.trim().to_string();
            if token.is_empty() {
                return None;
            }
            // Bounded by the probe's own deadline: an instance that accepts
            // and never answers ends this round, and the wait, rather than
            // parking it in a read for ever.
            let mut ctl = connect_within(sock, &token, DEADLINE).ok()?;
            ctl.request("version").ok()?.ok().then_some(token)
        })
    }
}

/// A second instance a [`World`] hosts beside its own ([`World::sibling_instance`]),
/// reaped with the test.
pub struct Sibling {
    child: Child,
    /// Its per-pid control socket.
    pub sock: String,
    /// Its Owner token.
    pub token: String,
}

impl Drop for Sibling {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A successor instance ([`World::successor_instance`]): killed and reaped
/// with the test unless [`Successor::take`] hands it to the [`World`] it
/// succeeds, which then tears it down as its own.
pub struct Successor {
    child: Option<OwnedChild>,
    lifeline: Option<aterm_uds::lifeline::Lifeline>,
    /// Its control socket, in its own runtime dir.
    pub sock: String,
    /// Its Owner token.
    pub token: String,
    /// Where its stdout and stderr go (its bridge's too).
    pub log: PathBuf,
}

impl Successor {
    /// A fresh authenticated control connection to it.
    pub fn ctl(&self) -> Ctl {
        connect(&self.sock, &self.token).expect("connect to the successor aterm")
    }

    /// Every session it hosts, as `(local, sid, nonce)`.
    pub fn sessions(&self) -> Vec<(u64, String, String)> {
        let reply = self
            .ctl()
            .request("sessions")
            .unwrap_or_else(|e| panic!("sessions on the successor: {e}"));
        session_rows(&reply)
    }

    /// Its `fabric status` header, or empty when it does not answer.
    pub fn fabric_status(&self) -> String {
        connect(&self.sock, &self.token)
            .and_then(|mut c| c.request("fabric status"))
            .map(|r| r.header().to_string())
            .unwrap_or_default()
    }

    /// Make it `w`'s own instance: its process, its control socket, its token
    /// and its log replace the ones `w` held, whose process must be gone.
    pub fn take(mut self, w: &mut World) {
        w.gui = self.child.take();
        *w.lifeline
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = self.lifeline.take();
        w.stopped = false;
        w.ctl_sock = std::mem::take(&mut self.sock);
        w.token = std::mem::take(&mut self.token);
        w.gui_log = std::mem::take(&mut self.log);
    }
}

impl Drop for Successor {
    fn drop(&mut self) {
        self.lifeline.take();
        if let Some(mut child) = self.child.take()
            && let Err(why) = end_owned_child(&mut child)
        {
            eprintln!("successor cleanup unknown: {why}");
        }
    }
}

/// The `$HOME` a spawned aterm gets: inside the test's own scratch world, never
/// the developer's.
///
/// THIS IS WHY THE macOS CONSENT DIALOG USED TO STORM. `atpkg`'s store prefix is
/// derived from `$HOME` (`atpkg::store::default_prefix` via
/// `aterm_types::dirs::home_dir`, which reads `$HOME` first), so a spawned
/// `target/debug/aterm` inherited the real one and read
/// `~/Library/Application Support/aterm` — the container macOS attributes to the
/// SIGNED `/Applications/aterm.app`. The binary this harness spawns is unsigned,
/// so TCC sees a stranger reaching into another app's data and asks the operator,
/// with aterm's own `NSAppDataUsageDescription` string. The grant cannot stick:
/// an unsigned binary has no stable code identity, so every rebuild is a new
/// stranger. MEASURED 2026-08-31: one full run of this crate's e2e suites spawns
/// dozens of GUIs and produced a dialog storm the operator had to click through.
///
/// A scratch `$HOME` fixes it at the cause: nothing here needs the developer's
/// home — every path these tests read is under `tmp`.
fn scratch_home(tmp: &std::path::Path) -> std::path::PathBuf {
    let home = tmp.join("home");
    // Created eagerly: a `$HOME` that does not exist is a different failure mode
    // from a `$HOME` that is empty, and only the second one is what we mean.
    let _ = std::fs::create_dir_all(&home);
    home
}

/// `cmd`, a one-shot `aterm link …` of the world at `tmp`, given that world's
/// per-user folders ([`scratch_home`], and its `run` and `cfg`) and none of this
/// process's `ATERM_*`: a CLI child reads `aterm.toml` and the state dir's
/// `fabric.toml` as an instance does, and on the inherited `$HOME` those are the
/// person's (2026-10-18: `link ls --fleet` and an observer's `link serve --fleet`
/// read them).
pub fn scratch_cli<'a>(cmd: &'a mut Command, tmp: &Path) -> &'a mut Command {
    prepare_gui_environment(cmd);
    cmd.envs(scratch_cli_env(tmp))
}

/// [`scratch_cli`] for a child built with `aterm_uds::spawnfd::command`.
pub fn scratch_spawnfd<'a>(
    cmd: &'a mut aterm_uds::spawnfd::Command,
    tmp: &Path,
) -> &'a mut aterm_uds::spawnfd::Command {
    for (name, _) in std::env::vars_os() {
        if name.to_string_lossy().starts_with("ATERM_") {
            cmd.env_remove(&name);
        }
    }
    cmd.env(STALE_HORIZON.0, STALE_HORIZON.1.as_millis().to_string());
    cmd.envs(scratch_cli_env(tmp))
}

/// [`scratch_cli`]'s folders.
fn scratch_cli_env(tmp: &Path) -> [(&'static str, PathBuf); 3] {
    [
        ("HOME", scratch_home(tmp)),
        ("XDG_RUNTIME_DIR", tmp.join("run")),
        ("XDG_CONFIG_HOME", tmp.join("cfg")),
    ]
}

// A5 — a FLEET: one broker, and two or more nodes on it

/// One guarded broker served on a Unix socket in the fleet's scratch dir, and
/// the nodes the tests boot against it: what needs two NODES, which one
/// [`World`] cannot be. Each node is its own headless aterm with its own
/// bridge child, as two aterms on one machine are; the bus is one machine's.
pub struct Fleet {
    pub tmp: PathBuf,
    broker: Option<Broker>,
    handle: Option<BrokerHandle>,
    /// The broker's socket, `<tmp>/b.sock`: `tmp` is per test and process, so
    /// parallel runs never clash.
    pub sock: String,
    next: u32,
    /// The read connection every polled broker read shares ([`Fleet::read`]).
    watch: std::sync::Mutex<Option<aterm_link::transport::Conn>>,
    /// Exclusive scratch ownership, dropped after the broker and read connection.
    scratch: Option<aterm_tempfile::TempDir>,
}

impl Drop for Fleet {
    fn drop(&mut self) {
        if let Some(mut h) = self.handle.take() {
            h.shutdown();
        }
        self.broker.take();
        // `$ATERM_LINK_KEEP=1` leaves the scratch world behind. A failing
        // end-to-end test whose evidence deletes itself is a test nobody can
        // diagnose; this is the one escape hatch, and it is off by default so a
        // green run leaves nothing.
        if std::env::var_os("ATERM_LINK_KEEP").is_some()
            && let Some(scratch) = self.scratch.take()
        {
            let _ = scratch.keep();
        }
    }
}

impl Fleet {
    /// Boot the broker under exclusive scratch ownership. The short fixed prefix
    /// keeps every node's control socket below Unix sockaddr path limits.
    pub fn boot(tag: &str) -> Self {
        Self::boot_opts(tag, broker_opts(aterm_link::durability::Durability::Strict))
    }

    /// [`Fleet::boot`] with the broker opened with `opts` ([`broker_opts`])
    /// instead of aterm's Strict options: the ordered suite's.
    pub fn boot_opts(_tag: &str, opts: OpenOptions) -> Self {
        let scratch = aterm_tempfile::Builder::new()
            .prefix("a5-")
            .tempdir_in("/tmp")
            .expect("exclusive fleet scratch");
        let tmp = scratch.path().to_path_buf();
        let broker = open_broker(tmp.join("b.log"), &opts);
        let sock = tmp.join("b.sock").to_string_lossy().into_owned();
        assert_sock_fits(&sock);
        let handle = broker.serve(&sock).expect("serve the broker's socket");
        Self {
            tmp,
            broker: Some(broker),
            handle: Some(handle),
            sock,
            next: 0,
            watch: std::sync::Mutex::new(None),
            scratch: Some(scratch),
        }
    }

    /// The broker this fleet serves, in this process: what a test reads the
    /// durable head off, and asks for a full flush.
    #[must_use]
    pub fn broker(&self) -> &Broker {
        self.broker.as_ref().expect("the fleet serves its broker")
    }

    /// A god-cap client on the socket the nodes use.
    pub fn god(&self) -> aterm_link::transport::Conn {
        let (mut c, _closer) =
            aterm_link::transport::connect(&self.sock, DEADLINE).expect("connect to the broker");
        let cap = astream_cap::mint(SECRET, &format!("/f/{FLEET}/>")).expect("mint the god cap");
        c.attach(&cap.filter, &cap.tag).expect("attach the god cap");
        c
    }

    /// [`World::read`] on the fleet's broker: one broker READ on this fleet's
    /// shared read connection, where a fresh one per poll is a connect as well
    /// as the attach ([`read_on`]).
    ///
    /// # Errors
    ///
    /// The connect, the attach, or `f` on a connection opened for it.
    pub fn read<T>(
        &self,
        f: impl FnMut(&mut aterm_link::transport::Conn) -> std::io::Result<T>,
    ) -> std::io::Result<T> {
        read_on(
            &self.watch,
            || {
                let (mut c, _closer) = aterm_link::transport::connect(&self.sock, DEADLINE)?;
                let cap = read_cap();
                c.attach(&cap.filter, &cap.tag)?;
                Ok(c)
            },
            f,
        )
    }

    /// [`World::row`] over [`Fleet::read`]'s connection.
    pub fn row(&self, subject: &str) -> Option<String> {
        let page = self.read(|c| c.last(subject, "", 8)).ok()?;
        exact_body(page, subject)
    }

    /// A cap file holding exactly `grants`, for a node id this fleet has not
    /// given a bridge — the rogue of §6.1, or an observer.
    pub fn cap_file(&self, name: &str, grants: &[String]) -> PathBuf {
        let path = self.tmp.join(format!("{name}.cap"));
        let caps: String = grants.iter().map(|g| cap_line(g)).collect();
        std::fs::write(&path, caps).expect("write a cap file");
        path
    }

    /// Boot one node: a headless `aterm` that launches its own real
    /// `aterm link serve` child, both pointed at this fleet's broker.
    pub fn node(&mut self, tag: &str, accept_from: &[&str]) -> Node {
        self.node_with(tag, accept_from, &[], &[])
    }

    /// [`Fleet::node`] with extra `aterm link serve` flags and extra environment
    /// on the `aterm` the bridge is a child of.
    ///
    /// The env goes on the GUI because that is who spawns the bridge: a fault
    /// marker or a screen flag set here reaches the process under test the way
    /// an operator's would, through the launch, rather than by a second path
    /// only the test knows about.
    pub fn node_with(
        &mut self,
        tag: &str,
        accept_from: &[&str],
        args: &[&str],
        env: &[(&str, &str)],
    ) -> Node {
        self.next += 1;
        Node::boot(self, tag, accept_from, args, env, self.next)
    }

    /// Run `aterm link ls` against this fleet and answer its stdout.
    ///
    /// THE REAL BINARY, on the fleet's broker, with a read-only cap —
    /// which is the whole point of the verb: one `Last` round trip and no
    /// filesystem scan, so a process that hosts nothing lists every node's
    /// sessions (§7).
    pub fn ls(&self) -> String {
        let cap = self.cap_file("ls", &[format!("ro:/f/{FLEET}/pub/>")]);
        let mut cmd = Command::new(aterm_to_launch());
        scratch_cli(&mut cmd, &self.tmp)
            .args(["link", "ls", "--fleet", FLEET, "--broker", &self.sock])
            .args(["--cap-file", cap.to_string_lossy().as_ref()]);
        let out = output_within(&mut cmd, "aterm link ls");
        assert!(
            out.status.success(),
            "aterm link ls failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }
}

/// One aterm instance on a [`Fleet`], with its own bridge child.
pub struct Node {
    pub tmp: PathBuf,
    pub ctl_sock: String,
    pub token: String,
    pub node: String,
    pub state: PathBuf,
    pub gui_log: PathBuf,
    pub cap_file: PathBuf,
    gui: Option<Child>,
    fabric_cmd: String,
    /// Extra environment for the `aterm` this node runs — and therefore for
    /// the bridge child it launches. Applied AFTER
    /// [`prepare_gui_environment`], which deliberately removes
    /// `ATERM_LINK_FAULT` from an inherited environment: a test that arms a
    /// fault must say so explicitly, and must never have one leak in from the
    /// aterm the test itself is running inside.
    extra_env: Vec<(String, String)>,
}

impl Drop for Node {
    fn drop(&mut self) {
        if let Some(mut gui) = self.gui.take() {
            let _ = gui.kill();
            let _ = gui.wait();
        }
        if let Ok(pid) = std::fs::read_to_string(self.state.join("pid"))
            && let Ok(pid) = pid.trim().parse::<i32>()
        {
            kill(pid, 9);
        }
    }
}

impl Node {
    fn boot(
        fleet: &Fleet,
        tag: &str,
        accept_from: &[&str],
        args: &[&str],
        env: &[(&str, &str)],
        ordinal: u32,
    ) -> Self {
        let _boot = boot_permit();
        let tmp = fleet.tmp.join(format!("n{ordinal}"));
        std::fs::create_dir_all(tmp.join("run/aterm")).expect("scratch runtime dir");
        prepare_fixture_config(&tmp);
        let state = tmp.join("link");
        std::fs::create_dir_all(&state).expect("bridge state dir");

        // PROVISIONED, not minted: a capability is bound to a principal, so the
        // operator mints for an id the node already has.
        let node = format!("n-{:016x}", fnv(tag));
        std::fs::write(state.join("node"), format!("{node}\n")).expect("seed the node id");

        let ctl_sock = tmp
            .join("run/aterm/aterm.sock")
            .to_string_lossy()
            .into_owned();
        assert_sock_fits(&ctl_sock);

        let cap_file = tmp.join("node.cap");
        let caps: String = node_grants(&node).iter().map(|g| cap_line(g)).collect();
        std::fs::write(&cap_file, caps).expect("write the node cap file");

        let mut fabric_cmd = format!(
            "{} link serve --fleet {FLEET} --broker {} --cap-file {} --state {}",
            aterm_to_launch(),
            fleet.sock,
            cap_file.display(),
            state.display()
        );
        if !accept_from.is_empty() {
            fabric_cmd.push_str(&format!(" --accept-from {}", accept_from.join(",")));
        }
        for arg in args {
            fabric_cmd.push(' ');
            fabric_cmd.push_str(arg);
        }
        std::fs::write(tmp.join("fabric.cmd"), &fabric_cmd).expect("record the fabric command");

        let gui_log = tmp.join("gui.log");
        let mut me = Self {
            tmp,
            ctl_sock,
            token: String::new(),
            node,
            state,
            gui_log,
            cap_file,
            gui: None,
            fabric_cmd,
            extra_env: env
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect(),
        };
        let gui = me.gui.insert(me.spawn_gui());
        me.token = World::wait_for_token_of(&me.ctl_sock, gui, &me.gui_log);
        me
    }

    fn spawn_gui(&self) -> Child {
        let out = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(&self.gui_log)
            .expect("gui log");
        let err = out.try_clone().expect("gui log clone");
        let mut cmd = Command::new(aterm_to_launch());
        // BEFORE the explicit `.env` calls below, never after: `Command`
        // applies env operations in order, so a blanket removal that ran last
        // would delete the three variables this harness deliberately sets.
        prepare_gui_environment(&mut cmd);
        cmd.arg("--headless")
            .env("HOME", scratch_home(&self.tmp))
            .env("TMPDIR", &self.tmp)
            .env("XDG_RUNTIME_DIR", self.tmp.join("run"))
            .env("XDG_CONFIG_HOME", self.tmp.join("cfg"))
            .env("SHELL", "/bin/sh")
            .args(["--lines", "40", "--columns", "120"])
            .env("ATERM_FABRIC_COMMAND", &self.fabric_cmd)
            .stdin(Stdio::null())
            .stdout(out)
            .stderr(err);
        for (k, v) in &self.extra_env {
            cmd.env(k, v);
        }
        arm_lifeline(&mut cmd, &self.tmp);
        cmd.spawn().expect("launch aterm --headless")
    }

    /// A fresh authenticated control connection.
    pub fn ctl(&self) -> Ctl {
        connect(&self.ctl_sock, &self.token).expect("connect to aterm")
    }

    /// Run one verb and return the reply.
    pub fn verb(&self, line: &str) -> Reply {
        self.ctl()
            .request(line)
            .unwrap_or_else(|e| panic!("{line}: {e}"))
    }

    /// Wait until the bridge is attached and its presence row says `live`.
    pub fn wait_ready(&self, fleet: &Fleet) {
        until("aterm to report fabric=connected", || {
            self.verb("status")
                .header()
                .contains("fabric=connected")
                .then_some(())
        });
        let subject = format!("/f/{FLEET}/pub/{}/node/presence", self.node);
        until("the node's presence row to say live", || {
            fleet
                .row(&subject)
                .filter(|b| b.contains("state=live"))
                .map(|_| ())
        });
    }

    /// Every session aterm hosts, as `(local, sid, nonce)`.
    pub fn sessions(&self) -> Vec<(u64, String, String)> {
        session_rows(&self.verb("sessions"))
    }

    /// The boot session's `(sid, nonce)`, once the bridge has seen it.
    pub fn session(&self) -> (String, String) {
        until("aterm's boot session", || {
            self.sessions()
                .first()
                .map(|(_, sid, nonce)| (sid.clone(), nonce.clone()))
        })
    }

    /// This session's `inbox` rows (`--peek`, so the watermarks do not move).
    pub fn inbox(&self, sid: &str) -> Vec<String> {
        inbox_rows(&self.verb(&format!("@{sid} inbox --peek")))
    }

    /// Every `ev` record this node has published, newest last — over EVERY `ev`
    /// face it owns. See [`World::ev`].
    pub fn ev(&self, fleet: &Fleet) -> Vec<String> {
        let filter = format!("/f/{FLEET}/pub/{}/>", self.node);
        fleet
            .read(|c| ev_rows(c, &filter))
            .unwrap_or_else(|e| panic!("read the ev records under {filter}: {e}"))
    }

    /// The tail of this node's aterm log, for a failure message.
    pub fn log_tail(&self) -> String {
        log_tail_of(&self.gui_log)
    }

    /// The bridge child's pid, from its state dir.
    pub fn bridge_pid(&self) -> i32 {
        until("the bridge to write its pid", || {
            std::fs::read_to_string(self.state.join("pid"))
                .ok()?
                .trim()
                .parse()
                .ok()
        })
    }

    /// QUIT this node's aterm the way a person does — `invoke Quit`, which a
    /// headless instance takes without a confirm and ends through the same
    /// teardown as Cmd-Q — and wait for the process to end, then for its
    /// bridge to follow it: once the bridge is gone nothing more is published
    /// under this node but its will.
    ///
    /// The verb's reply is not read: the instance may close the connection as
    /// it quits, and the process ending is the observable that counts.
    ///
    /// The exited bridge's `state/pid` is removed once it is gone: [`Drop`]
    /// SIGKILLs the pid that file names, and by the end of the test the
    /// number may belong to another process of this user.
    pub fn quit(&mut self) {
        let bridge = self.bridge_pid();
        let _ = self.ctl().request("invoke Quit");
        assert!(self.gui.is_some(), "this node's aterm is running");
        // Waited on in place: a quit that never lands panics with the child
        // still in `self.gui`, so [`Drop`] kills it rather than leaving it
        // running (and on the broker) for the rest of the test binary.
        until("aterm to quit", || {
            self.gui.as_mut()?.try_wait().ok().flatten()
        });
        self.gui = None;
        until("its bridge to exit after it", || {
            (!alive(bridge)).then_some(())
        });
        let _ = std::fs::remove_file(self.state.join("pid"));
    }
}
