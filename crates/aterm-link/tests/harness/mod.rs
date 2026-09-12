// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Shared by `bridge_e2e.rs`: one guarded broker, one headless aterm, one real
//! bridge child between them, and the bounded waits that keep the whole thing
//! deterministic.
//!
//! NOTHING HERE SLEEPS AS SYNCHRONISATION. Every wait is `until <observable
//! state>`, bounded by [`DEADLINE`] — and that bound is a HANG DETECTOR, not a
//! performance assertion: a healthy step here takes single-digit milliseconds,
//! and the bound only turns a wedged run into a diagnosable failure instead of a
//! `cargo test` that never returns.

#![allow(dead_code)]

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use astream_broker::{Broker, BrokerHandle, Client};
use aterm_link::ctl::{Ctl, Reply};

/// The mint secret every test's broker is guarded with.
pub const SECRET: &[u8] = b"a3-bridge-e2e-secret";

/// The fleet every test runs in.
pub const FLEET: &str = "f1";

/// The ceiling on every wait. Generous on purpose — see the module note.
pub const DEADLINE: Duration = Duration::from_secs(60);

/// How long a poll waits between observations of a process's state. Not a
/// synchronisation primitive: the assertion is the OBSERVATION, and this only
/// keeps the poll from spinning a core.
const POLL_GAP: Duration = Duration::from_millis(10);

/// `sockaddr_un.sun_path` is ~104 bytes on macOS/BSD. Every path here is built
/// short enough with margin, and this is the check that says so out loud rather
/// than failing deep inside `bind`.
const MAX_SOCK_PATH: usize = 100;

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
///
/// # Panics
///
/// After `budget`, naming what was being waited for.
pub fn until_within<T>(budget: Duration, what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + budget;
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(
            Instant::now() < deadline,
            "timed out after {budget:?} waiting for: {what}"
        );
        std::thread::sleep(POLL_GAP);
    }
}

/// The budget for something the bridge's periodic backstop publishes.
///
/// `bridge.rs`'s `ROSTER_REFRESH` is 2 s and the round is a bounded local walk,
/// so the honest bound is a handful of rounds plus whatever the machine is
/// doing. THREE MINUTES IS NOT A PERFORMANCE CLAIM — a healthy round lands in
/// milliseconds. It is [`DEADLINE`]'s argument applied to a slower clock: the
/// only thing the number decides is how long a WEDGED run takes to be named.
pub const PERIODIC_DEADLINE: Duration = Duration::from_secs(180);

/// How many full stacks may be COMING UP at once inside one test binary.
///
/// Every e2e test here boots a broker, a headless `aterm-gui` and the real
/// `aterm-link serve` child that gui launches, and `cargo test` runs the tests
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

/// Take a boot permit, waiting for one if [`max_concurrent_boots`] are already
/// out.
/// Refuse to start on a machine that is still running a PREVIOUS run's daemons.
///
/// Every world here spawns real `aterm-gui --headless` and `aterm-link serve` children.
/// When a run is killed rather than dropped — a `Ctrl-C`, a harness panic before `Drop`,
/// an agent's process being reaped — those children SURVIVE, holding sockets and CPU,
/// and the next run measures them instead of itself. That failure does not announce
/// itself: it looks exactly like a flaky test, then like a real regression, and it is
/// neither. It cost this project a bisect that reported every commit broken, and,
/// separately, two benchmark floors misdiagnosed as miscalibrated while 73 orphaned
/// busy-wait shells held the machine at load 90.
///
/// So the harness states the precondition it has always silently assumed. It is the same
/// doctrine `gui_binary` applies to a missing binary and `refuse_a_stale_binary` to an old
/// one: a run whose subject is not what it thinks is worth nothing, so refuse it loudly
/// with the command that fixes it rather than producing a number nobody can trust.
///
/// A STRAY IS A DAEMON OLDER THAN THIS PROCESS. That is the whole rule, and getting it
/// wrong makes the guard worse than nothing: `--all-targets` runs each test binary as its
/// own process, so a plain "is anything alive?" check fails binary two on binary one's
/// daemons still winding down, and a guard that fails the suite it protects is one people
/// learn to switch off. Comparing elapsed times separates them exactly — a daemon this
/// run started cannot be older than this run.
///
/// `$ATERM_LINK_ALLOW_STRAYS=1` opts out, for the one honest case — a developer
/// deliberately running two suites at once and willing to read the results with that in
/// mind.
fn refuse_a_dirty_machine() {
    // ONCE PER PROCESS, at the FIRST world's boot. This suite runs its tests in
    // parallel and every world spawns its own pair, so from the second boot onward
    // the daemons this run started are indistinguishable from a previous run's by
    // `pgrep` alone — checking every time turns a correct guard into one that fails
    // its own suite. At the first boot nothing of this run exists yet, so anything
    // alive then is genuinely someone else's.
    static ONCE: std::sync::Once = std::sync::Once::new();
    let mut found: Option<String> = None;
    ONCE.call_once(|| found = strays_now());
    let Some(strays) = found else {
        return;
    };
    panic!(
        "a PREVIOUS run's daemons were already alive when this suite started ({strays}). \
         This suite would measure them rather than itself, which reads as a flake and \
         then as a regression. Reap them (`pkill -f 'aterm-gui --headless'; \
         pkill -f 'aterm-link serve'`) and clear the scratch worlds \
         (`rm -rf /private/tmp/a5-* /private/tmp/atl-*`), or set \
         $ATERM_LINK_ALLOW_STRAYS=1 if the overlap is deliberate."
    );
}

/// A process's elapsed seconds, from `ps -o etime=`.
///
/// NOT `-o etimes=`. That keyword is Linux procps; BSD `ps` on macOS answers
/// `ps: etimes: keyword not found`. The first version of this guard used it, so every
/// age parse failed, `my_age` fell back to 0, every candidate's age came back `None`
/// and was filtered away, and the stray count was unconditionally zero — a guard that
/// could not fire, shipped as a guard. It survived because a guard that never fires
/// and a machine that is always clean look identical from the outside. Hence
/// [`self_check_age_reader`], which asserts this function can read a real process.
///
/// `etime` is POSIX and prints `[[dd-]hh:]mm:ss`.
fn process_age_secs(pid: &str) -> Option<i64> {
    let out = std::process::Command::new("ps")
        .args(["-o", "etime=", "-p", pid])
        .output()
        .ok()?;
    let raw = String::from_utf8_lossy(&out.stdout);
    let text = raw.trim();
    if text.is_empty() {
        return None; // no such process
    }
    let (days, clock) = match text.split_once('-') {
        Some((d, rest)) => (d.parse::<i64>().ok()?, rest),
        None => (0, text),
    };
    let mut parts = clock.split(':').rev();
    let secs: i64 = parts.next()?.trim().parse().ok()?;
    let mins: i64 = parts.next().unwrap_or("0").trim().parse().ok()?;
    let hours: i64 = parts.next().unwrap_or("0").trim().parse().ok()?;
    Some(days * 86_400 + hours * 3_600 + mins * 60 + secs)
}

/// The stray daemons alive RIGHT NOW, or `None` when the machine is clean (or when the
/// question cannot be asked, in which case the harness says nothing rather than guessing).
fn strays_now() -> Option<String> {
    if std::env::var_os("ATERM_LINK_ALLOW_STRAYS").is_some() {
        return None;
    }
    // How long THIS process has been alive. Anything older predates the run.
    let Some(my_age) = process_age_secs(&std::process::id().to_string()) else {
        return None; // cannot ask: say nothing rather than guess
    };
    let mut strays = Vec::new();
    for pattern in ["aterm-gui --headless", "aterm-link serve"] {
        let Ok(out) = std::process::Command::new("pgrep")
            .arg("-f")
            .arg(pattern)
            .output()
        else {
            return None;
        };
        let older = String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|pid| process_age_secs(pid.trim()))
            // A couple of seconds of slack: our own children are born a moment after we
            // are, and ps reports whole seconds.
            .filter(|age| *age > my_age + 2)
            .count();
        if older > 0 {
            strays.push(format!("{older} x `{pattern}`"));
        }
    }
    (!strays.is_empty()).then(|| strays.join(", "))
}

pub fn boot_permit() -> BootPermit {
    refuse_a_dirty_machine();
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

/// Mint one capability and render it as a `--cap-file` line.
pub fn cap_line(grant: &str) -> String {
    let cap = astream_cap::mint(SECRET, grant).expect("mint");
    let hex: String = cap.tag.iter().map(|b| format!("{b:02x}")).collect();
    format!("{} {hex}\n", cap.filter)
}

/// The eight grants a node needs (§3.3, §8.2's "A node's ring"), one per face it
/// touches.
pub fn node_grants(node: &str) -> Vec<String> {
    vec![
        // its own `pub/<node>/` subtree: presence, ev, say, ack, control
        format!("rw,p={node}:/f/{FLEET}/pub/{node}/>"),
        // every member's presence and digest
        format!("ro:/f/{FLEET}/pub/>"),
        // the fleet broadcast face — read only: a node holds NO halt authority
        format!("ro:/f/{FLEET}/fleet/>"),
        // talk to anyone, AS this node, any kind — the `*` kind segment is what
        // pins an `in` subject to seven segments
        format!("rw,p={node}:/f/{FLEET}/in/*/*/{node}/*"),
        // read its own inbox lanes
        format!("ro:/f/{FLEET}/in/{node}/>"),
        // its consumer-group NAMES (never published to, never delivered)
        format!("rw,p={node}:/f/{FLEET}/cur/{node}/>"),
        // the drive face for the sessions it hosts — READ ONLY, and that split
        // is the point: a node that could write `term/<n>/*/in/<src>` could
        // forge a driver into its own sessions (§8.2)
        format!("ro:/f/{FLEET}/term/{node}/>"),
        // the SCREEN face for the sessions it hosts — write only, and only used
        // when a node was started with `--screen`
        format!("rw,p={node}:/f/{FLEET}/term/{node}/*/screen"),
    ]
}

/// The six grants a HUMAN needs (§8.2's "A human's ring"), one per face they
/// touch. A human is not a node: they host no session, own no `pub/<n>/`
/// subtree, and hold no `term/<n>/>` read grant. What they do hold that a node
/// never may is the fleet halt and the drive lane keyed to their OWN principal —
/// `rw,p=<h>:/f/<F>/term/*/*/in/<h>` — which is the split that stops a node
/// forging a driver into its own sessions.
pub fn human_grants(h: &str) -> Vec<String> {
    vec![
        // the fleet halt, theirs alone
        format!("rw,p={h}:/f/{FLEET}/fleet/{h}/>"),
        // talk to anyone, AS this human, any kind
        format!("rw,p={h}:/f/{FLEET}/in/*/*/{h}/*"),
        // the drive face, keyed to this human
        format!("rw,p={h}:/f/{FLEET}/term/*/*/in/{h}"),
        // their own read lane, and the cursor name that drains it
        format!("ro:/f/{FLEET}/in/p/{h}/>"),
        format!("rw,p={h}:/f/{FLEET}/cur/p/{h}/>"),
        // every member's presence and digest
        format!("ro:/f/{FLEET}/pub/>"),
    ]
}

/// A HUMAN AT A THIRD CLIENT — a phone, a laptop, anything that is not one of
/// the fleet's nodes (§6.6's story B4).
///
/// It holds a minted `h-*` ring and one sealed broker connection, and it has no
/// aterm at all: everything it does is a record. That is the point of the type —
/// a test that drove a session through a `Node`'s control socket would be
/// testing aterm, not the fabric, and every rung that proved a human "took
/// control" that way would have proved nothing about the bus.
///
/// The producer id is DERIVED from the principal, not chosen: the broker binds
/// `rw,p=<h>:` grants to `producer_id_of(h)` and refuses a publish under any
/// other (R4's dedup-poisoning rule), so a `Human` that picked its own number
/// would be refused at the first publish.
pub struct Human {
    pub name: String,
    conn: aterm_link::transport::Conn,
    producer: u64,
    seq: u64,
}

impl Human {
    /// Mint this human's ring and attach it over the fleet's sealed wire.
    pub fn arrive(fleet: &Fleet, name: &str) -> Self {
        let (mut conn, closer) =
            aterm_link::transport::connect(&fleet_transport(fleet.key()), &fleet.addr)
                .expect("the human reaches the broker over the sealed wire");
        // The closer is deliberately dropped: this connection lives as long as
        // the `Human` and is closed by the socket going away with it.
        drop(closer);
        for grant in human_grants(name) {
            let cap = astream_cap::mint(SECRET, &grant).expect("mint a human grant");
            conn.attach(&cap.filter, &cap.tag)
                .unwrap_or_else(|e| panic!("attach {grant}: {e}"));
        }
        Self {
            name: name.to_string(),
            conn,
            producer: astream_cap::producer_id_of(name),
            seq: 0,
        }
    }

    /// Publish one record as this human, answering the offset it landed at.
    pub fn publish(&mut self, subject: &str, body: &[u8]) -> u64 {
        self.seq += 1;
        let (off, _) = self
            .conn
            .publish(self.producer, self.seq, subject, body)
            .unwrap_or_else(|e| panic!("publish {subject}: {e}"));
        off
    }

    /// A `control` message on a session's inbox lane (§6.6): `claim`, `request`,
    /// `release` or `grant <p>`. `text=` is one whitespace-delimited body token,
    /// so a two-word op is pct-encoded like every other body text (§4.1).
    pub fn control(&mut self, node: &str, sid: &str, epoch: &str, op: &str) -> u64 {
        let lane = format!("/f/{FLEET}/in/{node}/{sid}/{}/control", self.name);
        let body = format!("v=1 t=1 epoch={epoch} text={}", op.replace(' ', "%20"));
        self.publish(&lane, body.as_bytes())
    }

    /// A keystroke on a session's drive face, bound to the epoch and the
    /// generation the human read — and carrying `re=` as CAUSALITY, the offset
    /// of the ask this keystroke is the answer to.
    ///
    /// `re=` is carried, never consulted: §6.6 is explicit that a body which
    /// could trigger keystrokes on the strength of a `re=` would let any
    /// lane-writer drive a worker, so the bridge's four conditions are the
    /// holder, the hold, the epoch and the generation, and nothing else.
    pub fn drive(
        &mut self,
        node: &str,
        sid: &str,
        epoch: &str,
        gen: &str,
        re: Option<u64>,
        bytes: &[u8],
    ) -> u64 {
        let lane = format!("/f/{FLEET}/term/{node}/{sid}/in/{}", self.name);
        let re = re.map(|r| format!(" re={r}")).unwrap_or_default();
        let mut body =
            format!("v=1 t=1 epoch={epoch} gen={gen}{re} len={}\n", bytes.len()).into_bytes();
        body.extend_from_slice(bytes);
        self.publish(&lane, &body)
    }

    /// Everything on this human's own read lane, oldest first, as
    /// `(offset, subject, body)`. A bounded `Fetch`, not a drain: the point is to
    /// SEE the mail, and a cursor would make two reads of the same exchange
    /// disagree.
    pub fn lane(&mut self) -> Vec<(u64, String, Vec<u8>)> {
        let filter = format!("/f/{FLEET}/in/p/{}/>", self.name);
        let mut out = Vec::new();
        let mut from = 0u64;
        loop {
            let Ok((rows, (next, head))) = self.conn.fetch(from, &filter, 256) else {
                return out;
            };
            out.extend(rows);
            if next >= head || next <= from {
                return out;
            }
            from = next;
        }
    }
}

/// THE AUDITOR — one read-only capability over the whole fleet, `ro:/f/<F>/>`,
/// which is what §10's replay ("`Subscribe{0, /f/<F>/>}` … or `Fetch` paged")
/// actually needs.
///
/// It is a SEPARATE principal from the human on purpose, and the separation is
/// §8.2's rather than this harness's. A human's ring is
/// `rw,p=<h>:/f/<F>/fleet/<h>/>` · `rw,p=<h>:/f/<F>/in/*/*/<h>/*` ·
/// `rw,p=<h>:/f/<F>/term/*/*/in/<h>` · `ro:/f/<F>/in/p/<h>/>` ·
/// `rw,p=<h>:/f/<F>/cur/p/<h>/>` · `ro:/f/<F>/pub/>` — it can halt the fleet,
/// write to anyone, drive under its own name and read the roster, and it
/// **cannot read another session's mail or any screen**. Reading the whole log
/// back is a different authority, and a rung that had handed the human a fleet
/// read grant to make its own replay convenient would have quietly widened the
/// one capability this design is most careful about.
pub struct Auditor {
    conn: aterm_link::transport::Conn,
}

impl Auditor {
    /// Attach `ro:/f/<F>/>` over the fleet's sealed wire.
    pub fn arrive(fleet: &Fleet) -> Self {
        let (mut conn, closer) =
            aterm_link::transport::connect(&fleet_transport(fleet.key()), &fleet.addr)
                .expect("the auditor reaches the broker over the sealed wire");
        drop(closer);
        let cap =
            astream_cap::mint(SECRET, &format!("ro:/f/{FLEET}/>")).expect("mint the audit cap");
        conn.attach(&cap.filter, &cap.tag)
            .expect("attach the audit cap");
        Self { conn }
    }

    /// The broker's head offset — a fleet CUT on the bus, which §10 says is one
    /// offset because one broker gives the fleet a single spine.
    pub fn head(&mut self) -> u64 {
        let (_, (_, head)) = self
            .conn
            .fetch(0, &format!("/f/{FLEET}/>"), 0)
            .expect("the head query");
        head
    }

    /// Every record under `/f/<F>/` up to and including `cut`, oldest first —
    /// the replay §10 names (`Fetch` paged from zero), stopped at the cut.
    pub fn replay(&mut self, cut: u64) -> Vec<(u64, String, Vec<u8>)> {
        let filter = format!("/f/{FLEET}/>");
        let mut out: Vec<(u64, String, Vec<u8>)> = Vec::new();
        let mut from = 0u64;
        while let Ok((rows, (next, head))) = self.conn.fetch(from, &filter, 256) {
            for row in rows {
                if row.0 <= cut {
                    out.push(row);
                }
            }
            if next >= head || next <= from || next > cut {
                break;
            }
            from = next;
        }
        out
    }
}

/// THE INHERITED IDENTITY MUST BE STRIPPED, and this is not hygiene — it is the
/// difference between a two-node test and a one-node test wearing two hats.
///
/// aterm's ROOT session ADOPTS `$ATERM_SESSION_ID` / `$ATERM_LAUNCH_NONCE` when
/// they are set (`spawn.rs:783-786`, `adopt_injected_identity`), which is how an
/// outer aterm hands an inner one the identity its preminted edges already name.
/// A test run FROM INSIDE an aterm therefore has both variables in its own
/// environment, and every `aterm-gui` it spawns adopts the SAME sid and the SAME
/// launch nonce — two nodes hosting one sid, a bare `@s-<sid>` that resolves to
/// the sender itself, and a "relaunch" whose successor has the identical epoch.
/// Every one of those is a silent pass or a silent fail depending on which way
/// the assertion points.
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
/// the caller re-sets `ATERM_LINES`, `ATERM_COLUMNS` and `ATERM_FABRIC_COMMAND`
/// after this runs. A variable the test wants is one the test states; a variable
/// it inherits is one nobody chose.
///
/// AND THE PRICE OF THAT TRADE, stated because the next deny-by-default written
/// in this tree will meet it: **a deny-by-default that runs after its allow-list
/// deletes the allow-list.** `Command` applies env operations in order, so this
/// must be called immediately after `Command::new` and never at the end of the
/// builder chain — which is where all three call sites had the enumerated
/// version, harmlessly, because a list of eleven names it did not set could not
/// collide with three it did. Converting an enumeration into a derivation buys
/// completeness and takes on SEQUENCING as a new obligation; the enumerated
/// version's one virtue was that it could not get the order wrong. Get it wrong
/// here and three suites go red with no error pointing anywhere near the cause.
fn strip_inherited_identity(cmd: &mut Command) {
    for (name, _) in std::env::vars_os() {
        if name.to_string_lossy().starts_with("ATERM_") {
            cmd.env_remove(&name);
        }
    }
}

/// One booted world: a guarded broker, a headless aterm, and the bridge child
/// aterm launched. Torn down on every exit path, panics included.
pub struct World {
    pub tmp: PathBuf,
    pub broker: Option<Broker>,
    pub handle: Option<BrokerHandle>,
    pub broker_sock: String,
    pub broker_log: String,
    pub gui: Option<Child>,
    pub ctl_sock: String,
    pub token: String,
    pub node: String,
    pub state: PathBuf,
    pub gui_log: PathBuf,
}

impl Drop for World {
    fn drop(&mut self) {
        if let Some(mut gui) = self.gui.take() {
            let _ = gui.kill();
            let _ = gui.wait();
        }
        if let Some(mut h) = self.handle.take() {
            h.shutdown();
        }
        self.broker.take();
        // Kill the bridge too: aterm's supervisor relaunches it, so a leaked
        // child would outlive the test and keep publishing.
        if let Ok(pid) = std::fs::read_to_string(self.state.join("pid")) {
            if let Ok(pid) = pid.trim().parse::<i32>() {
                kill(pid, 9);
            }
        }
        // `$ATERM_LINK_KEEP=1` leaves the scratch world behind, for the same
        // reason [`Fleet::drop`] honours it and with the same default: a failing
        // end-to-end test whose gui log, cap file and bridge state dir delete
        // themselves is a test nobody can diagnose. `Fleet` has had the hatch
        // since it was written; `World` deleted unconditionally — and seven of
        // this crate's eleven end-to-end suites are `World` suites (bridge_e2e,
        // hooks, mirror and the four r1_*, 43 of its 60 e2e tests), so for most
        // of them the evidence was unreachable.
        if std::env::var_os("ATERM_LINK_KEEP").is_none() {
            let _ = std::fs::remove_dir_all(&self.tmp);
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

/// The `aterm-gui` binary. `$ATERM_GUI_BIN` names it explicitly; otherwise the
/// sibling workspace's debug build, which is where `targo test -p aterm-gui`
/// leaves it.
///
/// `aterm-gui` is a SIBLING crate, not a dependency of this one, so there is no
/// `CARGO_BIN_EXE_aterm-gui` to read: the binary has to be found rather than
/// declared. A missing one is a hard failure with the command to produce it, not
/// a skip — a rung that silently passed because its subject was not built would
/// be worth nothing.
///
/// THE WORKSPACE ROOT IS TWO LEVELS UP, not one. Until 2026-09-10 this crate was
/// its own workspace at the repo root, so `CARGO_MANIFEST_DIR/..` WAS the root;
/// it is now `crates/aterm-link`, and the one-level walk pointed at
/// `crates/target`, a directory that has never existed. Every e2e test failed
/// with "build it first" against a binary that was already built.
/// The `--tcp [--key-file …]` flags a CHILD process needs to reach this fixture,
/// matched to how [`Fleet::boot`] served it. Without `sealed` the fixture serves
/// plaintext loopback, and a child handed `--key-file` would be refused by its
/// own `transport::connect` — which is how every non-sealed e2e test timed out
/// on "the node's presence row to say live" the first time this was gated.
#[must_use]
pub fn fleet_child_flags(key_file: &std::path::Path) -> Vec<String> {
    #[cfg(feature = "sealed")]
    {
        vec![
            "--tcp".to_string(),
            "--key-file".to_string(),
            key_file.to_string_lossy().into_owned(),
        ]
    }
    #[cfg(not(feature = "sealed"))]
    {
        let _ = key_file;
        vec!["--tcp".to_string()]
    }
}

/// The transport this fixture's clients dial with, matched to how [`Fleet::boot`]
/// served it. `sealed` is astream's XChaCha20-Poly1305 record layer, which only
/// `two_nodes_sealed` asserts; a default build serves and dials plaintext
/// loopback, so every OTHER test here still runs without a cipher tree.
#[must_use]
pub fn fleet_transport(key: [u8; 32]) -> aterm_link::transport::Transport {
    #[cfg(feature = "sealed")]
    {
        aterm_link::transport::Transport::Sealed(Box::new(key))
    }
    #[cfg(not(feature = "sealed"))]
    {
        let _ = key;
        aterm_link::transport::Transport::Tcp
    }
}

/// The repository root: `crates/aterm-link` -> `crates` -> the workspace.
fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .expect("crates/aterm-link sits two levels under the workspace root")
        .to_path_buf()
}

pub fn gui_binary() -> PathBuf {
    if let Ok(p) = std::env::var("ATERM_GUI_BIN") {
        return PathBuf::from(p);
    }
    let root = workspace_root();
    for profile in ["debug", "release"] {
        let p = root.join("target").join(profile).join("aterm-gui");
        if p.exists() {
            refuse_a_stale_binary(&p, "aterm-gui", "ATERM_GUI_BIN");
            return p;
        }
    }
    panic!(
        "aterm-gui was not found under {}/target — build it first \
         (`targo --unverified build -p aterm-gui`) or set $ATERM_GUI_BIN",
        root.display()
    );
}

/// Refuse a binary OLDER than ANY source that compiles into it — for EVERY binary
/// this crate's suites FIND rather than declare.
///
/// [`gui_binary`] FINDS a binary; it cannot build one, because aterm-link is its own
/// workspace (the design's §11.2 rule) and cargo therefore has no dependency edge to
/// aterm-gui. So an e2e run here drives whatever the last `-p aterm-gui` left behind —
/// and anyone who edits `crates/aterm-gui` and immediately runs a test in this crate is
/// measuring the PREVIOUS binary with their own test file compiled fresh against it.
///
/// That is not hypothetical. It was caught with the binary four to six minutes behind
/// two of the three files a red suite was being used to accuse, and it cost hours of
/// chasing a defect in code that was not running.
///
/// `gui_binary`'s own doc already argues the principle for the MISSING case: "a rung
/// that silently passed because its subject was not built would be worth nothing". A
/// STALE subject is the same failure in better clothes — worse, because it looks built.
/// So this refuses it the same way, with the same command to fix it.
///
/// `$ATERM_GUI_BIN` is exempt: naming a binary explicitly is a deliberate act (the CI
/// lane and the cross-worktree runs both do it), and second-guessing it would break the
/// one case where the caller genuinely knows better.
///
/// ## IT IS EVERY SOURCE IN THE BINARY, NOT ONE CRATE'S
///
/// This used to walk exactly `crates/aterm-gui/src`, which is not "the sources it is
/// supposed to be": `aterm-gui` links ~80 local crates, and an edit to any of them is a
/// stale binary the guard could not see. Two of the modules the round it was written for
/// was auditing lived in them — `crates/aterm-uds/src/spawnfd.rs` and
/// `crates/aterm-types/src/control_verbs.rs` — so the guard added to stop an auditor
/// chasing a defect in code that is not running did not cover half of what was being
/// audited.
///
/// The input set is CARGO'S OWN, read from the depfile it writes beside the binary
/// (`target/<profile>/aterm-gui.d`): one make-style line naming every file the compile
/// consumed, transitively, assets and generated sources included. Deriving it any other
/// way — a manifest walk, `crates/*/src` — would either miss an edge or name files that
/// are NOT inputs, and the second is worse than the first here: a file cargo does not
/// depend on stays newer than the binary no matter how many times you rebuild, so the
/// guard would fire forever and the suite would be bricked until somebody deleted it.
/// Cargo's list has exactly the property that makes the demand answerable — every path
/// on it, when touched, makes `cargo build -p aterm-gui` produce a new binary.
///
/// TWO KINDS OF ENTRY ARE SKIPPED, and both are build STAMPS rather than sources:
/// anything under a `.git` directory (`aterm-gui`'s build script depends on `.git/HEAD`
/// and `.git/index`, which churn on every `git add` in a shared worktree and would
/// demand a 134 MB relink before every e2e run), and anything under the binary's OWN
/// target root, which is a build script's OUT_DIR output rather than something a human
/// edits. The second is matched by PATH PREFIX, not by a component named `target`: a
/// crate is entitled to a `src/target/` module, and a guard that silently drops a real
/// source is the failure this whole function exists to stop.
///
/// If the depfile is missing — a binary copied in from elsewhere — the walk falls back
/// to `crates/<krate>/src`, which is narrower than the claim above and says so here
/// rather than pretending to a coverage it does not have.
///
/// ## AND IT IS EVERY FOUND BINARY, NOT JUST aterm-gui
///
/// `aterm-gui` was the one this was written for, so it was written as
/// `refuse_a_stale_binary` and wired into [`gui_binary`] alone. This crate FINDS a second
/// binary the same way and for the same reason — `glance_and_tui.rs`'s `ctl_binary`,
/// whose own doc invokes the same doctrine ("a rung that passed because its subject was
/// not built would be worth nothing") and then did not check the stale half. When that
/// was noticed, `target/debug/aterm-ctl` was FOUR AND A HALF HOURS behind
/// `crates/aterm-types/src/control_verbs.rs` — the very file the round was auditing —
/// and the glance/tui e2e was driving it green. One guard, taking the crate name and
/// the `$…_BIN` exemption as arguments, is what makes "every found binary" checkable
/// instead of "the one somebody remembered".
pub fn refuse_a_stale_binary(bin: &std::path::Path, krate: &str, env_var: &str) {
    if std::env::var_os(env_var).is_some() {
        return;
    }
    let Ok(built) = std::fs::metadata(bin).and_then(|m| m.modified()) else {
        return;
    };
    if let Some((changed, path)) = newest_input(bin, krate) {
        assert!(
            changed <= built,
            "{krate} is STALE: {} changed after the binary at {} was built. This suite \
             would drive the PREVIOUS {krate} with your test compiled fresh against it, \
             which reads as a defect in code that is not running. Rebuild it \
             (`targo --unverified build -p {krate}`) or name one with ${env_var}.",
            path.display(),
            bin.display()
        );
    }
}

/// The newest file that compiles into `bin`, and its path — cargo's depfile if there is
/// one, else the narrow `crates/<krate>/src` walk. See [`refuse_a_stale_binary`].
fn newest_input(bin: &std::path::Path, krate: &str) -> Option<(std::time::SystemTime, PathBuf)> {
    // A depfile that names NO source falls back rather than answering "nothing is
    // newer": an empty answer here disarms the guard, and a guard that disarms itself
    // on a file it could not read is the shape of the defect this whole function is.
    let from_depfile = std::fs::read_to_string(bin.with_extension("d"))
        .ok()
        .map(|text| depfile_inputs(&text, &target_root(bin)))
        .filter(|paths| !paths.is_empty());
    let paths = match from_depfile {
        Some(paths) => paths,
        None => {
            // Same two-level walk as `workspace_root`: this crate moved from the
            // repo root into `crates/` on 2026-09-10, so a one-level `..` plus
            // `crates` resolved to `crates/crates/<krate>/src`.
            let src = workspace_root().join("crates").join(krate).join("src");
            let mut found = Vec::new();
            let mut stack = vec![src];
            while let Some(dir) = stack.pop() {
                let Ok(entries) = std::fs::read_dir(&dir) else {
                    continue;
                };
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_dir() {
                        stack.push(path);
                    } else if path.extension().is_some_and(|e| e == "rs") {
                        found.push(path);
                    }
                }
            }
            found
        }
    };
    newest_of(&paths)
}

/// The inputs a cargo depfile names: every dependency of every rule, minus the TARGETS
/// (the paths left of a `:`) and minus the two stamp classes [`refuse_a_stale_binary`]
/// documents. A `\ ` is cargo's escape for a space inside a path.
///
/// `target_root` is the build directory this binary was written into; anything under it
/// is a build script's OUT_DIR output, not a source. It is passed rather than guessed
/// from a component NAMED `target`, because a crate is entitled to a module directory of
/// that name and a guard that silently drops a real source is the failure this whole
/// function exists to stop.
fn depfile_inputs(text: &str, target_root: &std::path::Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for line in text.lines() {
        // Everything up to the FIRST unescaped `:` is the rule's target(s). Cargo also
        // emits bare `path:` lines so make tolerates a deleted dependency; those have
        // nothing to the right and contribute nothing.
        let mut deps = line;
        let mut prev_backslash = false;
        for (i, c) in line.char_indices() {
            if c == ':' && !prev_backslash {
                deps = &line[i + 1..];
                break;
            }
            prev_backslash = c == '\\';
        }
        let mut current = String::new();
        let mut chars = deps.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '\\' if chars.peek() == Some(&' ') => {
                    chars.next();
                    current.push(' ');
                }
                // A trailing `\` continues the rule onto the next line.
                '\\' if chars.peek().is_none() => {}
                c if c.is_whitespace() => {
                    push_input(&mut out, std::mem::take(&mut current), target_root);
                }
                c => current.push(c),
            }
        }
        push_input(&mut out, current, target_root);
    }
    out
}

/// Keep one depfile token if it is a source rather than a build STAMP.
fn push_input(out: &mut Vec<PathBuf>, token: String, target_root: &std::path::Path) {
    if token.is_empty() {
        return;
    }
    let path = PathBuf::from(token);
    let stamp = path.components().any(|c| c.as_os_str() == ".git") || path.starts_with(target_root);
    if !stamp {
        out.push(path);
    }
}

/// The build directory a found binary was written into — `target/<profile>/<name>`'s
/// grandparent.
fn target_root(bin: &std::path::Path) -> PathBuf {
    bin.parent().and_then(std::path::Path::parent).map_or_else(
        || PathBuf::from("/dev/null/no-such-target-root"),
        PathBuf::from,
    )
}

/// The newest of `paths` by mtime. A path that cannot be stat'd is skipped: it is
/// either deleted (cargo will rebuild anyway) or unreadable, and neither is evidence of
/// staleness.
fn newest_of(paths: &[PathBuf]) -> Option<(std::time::SystemTime, PathBuf)> {
    let mut newest: Option<(std::time::SystemTime, PathBuf)> = None;
    for path in paths {
        if let Ok(t) = std::fs::metadata(path).and_then(|m| m.modified()) {
            if newest.as_ref().is_none_or(|(seen, _)| t > *seen) {
                newest = Some((t, path.clone()));
            }
        }
    }
    newest
}

impl World {
    /// Boot everything. `tag` keeps parallel tests off each other's paths, and
    /// `accept_from` is the bridge's `--accept-from` list.
    pub fn boot(tag: &str, accept_from: &[&str]) -> Self {
        Self::boot_with(tag, accept_from, &[])
    }

    /// [`World::boot`] with extra environment for the bridge child (the fault
    /// injection the crash tests arm).
    pub fn boot_with(tag: &str, accept_from: &[&str], env: &[(&str, &str)]) -> Self {
        // ONE PERMIT PER STACK COMING UP (see [`boot_permit`]). Dropped when this
        // function returns, which is after the control socket answers — so the
        // window it bounds is exactly the expensive one.
        let _boot = boot_permit();
        let tmp = PathBuf::from(format!("/tmp/atl-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("run/aterm")).expect("scratch runtime dir");
        std::fs::create_dir_all(tmp.join("cfg/aterm")).expect("scratch config dir");
        let state = tmp.join("link");
        std::fs::create_dir_all(&state).expect("bridge state dir");

        // THE NODE ID IS PROVISIONED, not minted. A capability is bound to a
        // principal, so an operator mints the node's caps for an id the node
        // already has — which is exactly what seeding the state dir models.
        let node = format!("n-{:016x}", fnv(tag));
        std::fs::write(state.join("node"), format!("{node}\n")).expect("seed the node id");

        let broker_sock = tmp.join("b.sock").to_string_lossy().into_owned();
        let broker_log = tmp.join("b.log").to_string_lossy().into_owned();
        let ctl_sock = tmp
            .join("run/aterm/aterm.sock")
            .to_string_lossy()
            .into_owned();
        for p in [&broker_sock, &ctl_sock] {
            assert!(
                p.len() < MAX_SOCK_PATH,
                "socket path too long for sun_path: {p}"
            );
        }

        let broker = Broker::open_guarded(&broker_log, SECRET.to_vec()).expect("guarded broker");
        let handle = broker.serve(&broker_sock).expect("serve the broker");

        let cap_path = tmp.join("node.cap");
        let caps: String = node_grants(&node).iter().map(|g| cap_line(g)).collect();
        std::fs::write(&cap_path, caps).expect("write the node cap file");

        let mut fabric_cmd = format!(
            "{} serve --fleet {FLEET} --broker {broker_sock} --cap-file {} --state {}",
            env!("CARGO_BIN_EXE_aterm-link"),
            cap_path.display(),
            state.display()
        );
        if !accept_from.is_empty() {
            fabric_cmd.push_str(&format!(" --accept-from {}", accept_from.join(",")));
        }

        std::fs::write(tmp.join("fabric.cmd"), &fabric_cmd).expect("record the fabric command");
        let gui_log = tmp.join("gui.log");
        let out = std::fs::File::create(&gui_log).expect("gui log");
        let err = out.try_clone().expect("gui log clone");
        let mut cmd = Command::new(gui_binary());
        // BEFORE the explicit `.env` calls below, never after: `Command`
        // applies env operations in order, so a blanket removal that ran last
        // would delete the three variables this harness deliberately sets.
        strip_inherited_identity(&mut cmd);
        cmd.arg("--headless")
            .env("HOME", scratch_home(&tmp))
            .env("XDG_RUNTIME_DIR", tmp.join("run"))
            .env("XDG_CONFIG_HOME", tmp.join("cfg"))
            .env("SHELL", "/bin/sh")
            .env("ATERM_LINES", "40")
            .env("ATERM_COLUMNS", "120")
            .env("ATERM_FABRIC_COMMAND", &fabric_cmd)
            .stdin(Stdio::null())
            .stdout(out)
            .stderr(err);
        for (k, v) in env {
            cmd.env(k, v);
        }
        let gui = cmd.spawn().expect("launch aterm-gui --headless");

        let mut world = World {
            tmp,
            broker: Some(broker),
            handle: Some(handle),
            broker_sock,
            broker_log,
            gui: Some(gui),
            ctl_sock,
            token: String::new(),
            node,
            state,
            gui_log,
        };
        world.token = World::wait_for_token_at(&world.ctl_sock);
        world
    }

    /// A fresh authenticated control connection.
    pub fn ctl(&self) -> Ctl {
        Ctl::connect(&self.ctl_sock, &self.token).expect("connect to aterm")
    }

    /// Run one verb and return the reply.
    pub fn verb(&self, line: &str) -> Reply {
        self.ctl()
            .request(line)
            .unwrap_or_else(|e| panic!("{line}: {e}"))
    }

    /// A god-cap broker client for the test itself: the fleet root, which may
    /// publish under any producer id. That is what lets a test forge the records
    /// the bridge must refuse.
    pub fn god(&self) -> Client {
        let mut c = Client::connect(&self.broker_sock).expect("connect to the broker");
        let cap = astream_cap::mint(SECRET, &format!("/f/{FLEET}/>")).expect("mint the god cap");
        c.attach(&cap.filter, &cap.tag).expect("attach the god cap");
        c
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
            let mut c = self.god();
            let (rows, _) = c.last(&subject, "", 8).ok()?;
            rows.iter()
                .find(|(_, s, _)| *s == subject)
                .filter(|(_, _, b)| String::from_utf8_lossy(b).contains("state=live"))
                .map(|_| ())
        });
    }

    /// Every session aterm hosts, as `(local, sid, nonce)`.
    pub fn sessions(&self) -> Vec<(u64, String, String)> {
        self.verb("sessions")
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

    /// This session's `inbox` rows (`--peek`, so the watermarks do not move).
    pub fn inbox(&self, sid: &str) -> Vec<String> {
        self.verb(&format!("@{sid} inbox --peek"))
            .rows()
            .iter()
            .filter(|r| r.starts_with("msg "))
            .cloned()
            .collect()
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
        let mut c = self.god();
        ev_rows(&mut c, &format!("/f/{FLEET}/pub/{}/>", self.node))
    }

    /// The tail of aterm's own log, for a failure message.
    pub fn log_tail(&self) -> String {
        let body = std::fs::read_to_string(&self.gui_log).unwrap_or_default();
        let lines: Vec<&str> = body.lines().collect();
        lines[lines.len().saturating_sub(20)..].join("\n")
    }
}

/// Every `ev` payload under `filter`, oldest first. Shared by [`World::ev`] and
/// [`Node::ev`] so the two can never read different faces.
fn ev_rows<C: EvFetch>(conn: &mut C, filter: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut from = 0u64;
    loop {
        let Ok((rows, (next, head))) = conn.fetch_rows(from, filter, 256) else {
            return out;
        };
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
            return out;
        }
        from = next;
    }
}

/// One page of a `Fetch`: the rows, then the mark's `(next, head)`.
pub type FetchPage = (Vec<(u64, String, Vec<u8>)>, (u64, u64));

/// The one thing [`ev_rows`] needs of a connection — the plain `Client` the
/// unix-socket world uses and the sealed `Conn` the fleet uses are different
/// types with the same `fetch`.
pub trait EvFetch {
    /// `Fetch` from `off` under `filter`, answering the rows and `(next, head)`.
    ///
    /// # Errors
    ///
    /// Whatever the underlying connection answers.
    fn fetch_rows(&mut self, off: u64, filter: &str, max: u32) -> std::io::Result<FetchPage>;
}

impl EvFetch for Client {
    fn fetch_rows(&mut self, off: u64, filter: &str, max: u32) -> std::io::Result<FetchPage> {
        self.fetch(off, filter, max)
    }
}

impl EvFetch for aterm_link::transport::Conn {
    fn fetch_rows(&mut self, off: u64, filter: &str, max: u32) -> std::io::Result<FetchPage> {
        self.fetch(off, filter, max)
    }
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
    /// Relaunch `aterm-gui` against the same scratch world — the same broker,
    /// the same bridge state dir, the same node id, the same fabric command.
    pub fn relaunch_gui(w: &World) -> Child {
        let fabric_cmd = std::fs::read_to_string(w.tmp.join("fabric.cmd"))
            .expect("the boot recorded its fabric command");
        let out = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(&w.gui_log)
            .expect("gui log");
        let err = out.try_clone().expect("gui log clone");
        let mut cmd = Command::new(gui_binary());
        // BEFORE the explicit `.env` calls below, never after: `Command`
        // applies env operations in order, so a blanket removal that ran last
        // would delete the three variables this harness deliberately sets.
        strip_inherited_identity(&mut cmd);
        cmd.arg("--headless")
            .env("HOME", scratch_home(&w.tmp))
            .env("XDG_RUNTIME_DIR", w.tmp.join("run"))
            .env("XDG_CONFIG_HOME", w.tmp.join("cfg"))
            .env("SHELL", "/bin/sh")
            .env("ATERM_LINES", "40")
            .env("ATERM_COLUMNS", "120")
            .env("ATERM_FABRIC_COMMAND", fabric_cmd.trim())
            .stdin(Stdio::null())
            .stdout(out)
            .stderr(err);
        cmd.spawn().expect("relaunch aterm-gui --headless")
    }

    /// The instance token for a control socket that is coming up.
    ///
    /// The token FILE existing is not enough — a dead instance leaves one
    /// behind, and the `latest` alias repoints only when the successor binds. So
    /// the wait ends on a connection that ANSWERS, which is the observable state
    /// the test actually depends on.
    ///
    /// It answers TWO verbs, not one: `version` for the identity in a refusal
    /// message and `status` for the one precondition
    /// [`refuse_a_gui_that_cannot_launch_a_bridge`] checks. Both on the same
    /// connection, in the same round, so the wait costs what it always did.
    pub fn wait_for_token_at(sock: &str) -> String {
        let sock = sock.to_string();
        let (token, version, status) =
            until("aterm-gui to bind and answer on its control socket", || {
                let path = aterm_uds::latest::token_path_for_sock(&sock)?;
                let token = std::fs::read_to_string(path).ok()?;
                let token = token.trim().to_string();
                if token.is_empty() {
                    return None;
                }
                let mut ctl = Ctl::connect(&sock, &token).ok()?;
                let version = ctl.request("version").ok()?;
                if !version.ok() {
                    return None;
                }
                let status = ctl.request("status").ok()?;
                status.ok().then(|| {
                    (
                        token,
                        version.header().to_string(),
                        status.header().to_string(),
                    )
                })
            });
        refuse_a_gui_that_cannot_launch_a_bridge(&version, &status);
        token
    }
}

/// The `$HOME` a spawned aterm gets: inside the test's own scratch world, never
/// the developer's.
///
/// THIS IS WHY THE macOS CONSENT DIALOG USED TO STORM. `atpkg`'s store prefix is
/// derived from `$HOME` (`atpkg::store::default_prefix` via
/// `aterm_types::dirs::home_dir`, which reads `$HOME` first), so a spawned
/// `target/debug/aterm-gui` inherited the real one and read
/// `~/Library/Application Support/aterm` — the container macOS attributes to the
/// SIGNED `/Applications/aterm.app`. The binary this harness spawns is unsigned,
/// so TCC sees a stranger reaching into another app's data and asks the operator,
/// with aterm's own `NSAppDataUsageDescription` string. The grant cannot stick:
/// an unsigned binary has no stable code identity, so every rebuild is a new
/// stranger. MEASURED 2026-08-31: one full run of this crate's e2e suites spawns
/// dozens of GUIs and produced a dialog storm the operator had to click through.
///
/// A scratch `$HOME` fixes it at the cause and is what a hermetic e2e wanted
/// anyway: the suites already redirect `XDG_RUNTIME_DIR` and `XDG_CONFIG_HOME`
/// into `tmp`, and `$HOME` was simply the one that was missed. Nothing here
/// needs the developer's home — every path these tests read is under `tmp`.
fn scratch_home(tmp: &std::path::Path) -> std::path::PathBuf {
    let home = tmp.join("home");
    // Created eagerly: a `$HOME` that does not exist is a different failure mode
    // from a `$HOME` that is empty, and only the second one is what we mean.
    let _ = std::fs::create_dir_all(&home);
    home
}

/// THE ONE THING THE ATERM UNDER TEST HAS TO BE ABLE TO DO before any suite in
/// this crate means anything: LAUNCH A BRIDGE.
///
/// THIS IS THE FOURTH CASE, and the first three do not reach it.
/// [`gui_binary`] refuses a MISSING binary; [`refuse_a_stale_binary`] refuses one
/// older than any source cargo compiled into it; [`refuse_a_dirty_machine`]
/// refuses a previous run's daemons. TWO GAPS SURVIVE all three, and both were measured
/// on 2026-08-31 rather than reasoned about:
///
///  * `$ATERM_GUI_BIN` is set. [`refuse_a_stale_binary`] deliberately exempts an
///    explicitly named binary — naming one is a decision, and a guard that
///    second-guesses it gets switched off. Pointed at a pre-launcher aterm, the
///    eleven `bridge_e2e` tests failed here in 1.95 s WITH this check and would
///    have taken eleven times [`DEADLINE`] without it.
///  * The binary's mtime is newer than every source and it still predates the
///    launcher — a copy, a restore, or a `release`-profile artifact. Measured:
///    a 2026-08-28 binary copied into place with a fresh mtime produced a
///    60.66 s timeout on one `bridge_e2e` test, and a 2.33 s named refusal with
///    this check.
///
/// WHAT SUCH A BINARY IS. `crates/aterm-gui/src/fabric_launch.rs` — the launcher
/// that reads `$ATERM_FABRIC_COMMAND` and spawns the bridge — landed in
/// a0970345c on 2026-08-29. The 0.63.0 binary above answers
/// `grep -ac ATERM_FABRIC_COMMAND` = 0: it spawns no bridge and never can. So a
/// test booted a full stack and waited the whole of [`DEADLINE`] for
/// `fabric=connected` that could not arrive — a red that reads as a broken
/// bridge and is nothing of the kind. Before fc48302c4 (2026-08-31) that was the
/// DEFAULT path and 53 of this crate's 60 e2e tests failed exactly that way at
/// ~60 s each; the stale-binary refusal closed the default, and this closes what
/// it exempts.
///
/// THE INSTANCE IS ASKED, not the binary inspected. An aterm carrying the
/// launcher always answers `status` with a `fabric=` token, in every state the
/// bridge can be in — measured 2026-08-31 against `target/debug/aterm-gui` at
/// f188df942eaf: `fabric=absent` with no command configured, `fabric=disconnected`
/// with `ATERM_FABRIC_COMMAND=/usr/bin/false`, `fabric=connected` with a child
/// that stays up. An aterm that predates the launcher has no such token in its
/// `status` header at all (measured on the 0.63.0 binary above), and that
/// absence is the whole test.
///
/// IT REFUSES, IT DOES NOT SKIP, and that half is deliberate. This machine can
/// run the suite — one `targo --unverified build -p aterm-gui` later the first
/// `bridge_e2e` test passed in 2.03 s — so the honest report is a fast, named
/// failure, not a green run whose subject was never built. It is the same
/// doctrine `gui_binary` applies to a missing binary and `refuse_a_stale_binary` to
/// an old one, applied to a binary that is present, fresh, and still cannot be
/// the subject.
///
/// AND IT SWALLOWS NOTHING. The token's PRESENCE is the only thing asserted, so
/// every real bridge failure on an aterm that can launch one — a bridge that
/// never starts (`fabric=disconnected`), one that starts and publishes nothing,
/// one configured away (`fabric=absent`) — passes this check untouched and fails
/// in [`World::wait_ready`] exactly as it does today.
///
/// # Panics
///
/// When the instance's `status` carries no `fabric=` token.
fn refuse_a_gui_that_cannot_launch_a_bridge(version: &str, status: &str) {
    assert!(
        status.contains("fabric="),
        "THE ATERM UNDER TEST CANNOT LAUNCH A BRIDGE: its `status` carries no \
         `fabric=` token, so this build predates crates/aterm-gui/src/fabric_launch.rs \
         and $ATERM_FABRIC_COMMAND reaches nothing in it. Every test in this crate \
         would boot a stack and then wait {DEADLINE:?} for a bridge that cannot start. \
         Build one (`targo --unverified build -p aterm-gui`) or point $ATERM_GUI_BIN at \
         one.\n  binary:  {}\n  version: {version}\n  status:  {status}",
        gui_binary().display()
    );
}

// ---------------------------------------------------------------------------
// A5 — a FLEET: one broker on the sealed wire, and two or more nodes on it
// ---------------------------------------------------------------------------

/// One broker served over `--tcp --key-file`: astream's XChaCha20-Poly1305
/// sealed record layer under a pre-shared key (§8.6).
///
/// LOOPBACK IS NOT A WEAKENING OF THE CLAIM. The sealed transport is a property
/// of the connection, not of the route: the same two 36-byte hellos, the same
/// per-connection AAD binding direction and record sequence, and the same
/// key-confirming record each way run whether the two ends are on one host or
/// two. What loopback buys is a test that needs no second machine; what it does
/// NOT test is a network that reorders, drops or delays — that is the
/// transport's own problem and astream's own tests.
pub struct Fleet {
    pub tmp: PathBuf,
    broker: Option<Broker>,
    handle: Option<BrokerHandle>,
    /// `127.0.0.1:<port>` — the port is ephemeral, so parallel runs never clash.
    pub addr: String,
    /// The 64-hex key file every node and `ls` is pointed at.
    pub key_file: PathBuf,
    key: [u8; 32],
    next: u32,
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
        if std::env::var_os("ATERM_LINK_KEEP").is_none() {
            let _ = std::fs::remove_dir_all(&self.tmp);
        }
    }
}

impl Fleet {
    /// Boot the broker. `tag` keeps parallel tests off each other's paths.
    pub fn boot(tag: &str) -> Self {
        let tmp = PathBuf::from(format!("/tmp/a5-{tag}{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).expect("scratch");
        // A FIXED KEY, not a random one: the test is about the wire being
        // sealed, and a key nobody can print is a test nobody can debug. It is
        // written in the same 64-hex shape `asb --key-file` reads, because one
        // key file has to feed the broker and every bridge on the fleet.
        let key = [0x5au8; 32];
        let key_file = tmp.join("fleet.key");
        let hex: String = key.iter().map(|b| format!("{b:02x}")).collect();
        std::fs::write(&key_file, format!("{hex}\n")).expect("write the key file");

        let broker = Broker::open_guarded(
            tmp.join("b.log").to_string_lossy().as_ref(),
            SECRET.to_vec(),
        )
        .expect("guarded broker");
        // THE WIRE, and the one place the cipher tree is optional. `sealed`
        // serves astream's XChaCha20-Poly1305 record layer, which is what
        // `two_nodes_sealed` asserts and the only rung that needs it. Without
        // the feature this fixture is still a real two-node TCP fleet — every
        // other test here uses it as "a broker on a socket" and asserts nothing
        // about confidentiality — so they keep running in a default build,
        // which is what lets a shipped binary carry no cipher at all.
        #[cfg(feature = "sealed")]
        let handle = broker
            .serve_tcp_sealed("127.0.0.1:0", key)
            .expect("serve the sealed wire");
        #[cfg(not(feature = "sealed"))]
        let handle = broker
            .serve_tcp("127.0.0.1:0")
            .expect("serve the plaintext loopback wire");
        let addr = handle
            .tcp_addr()
            .expect("a TCP endpoint answers its bound address")
            .to_string();
        Self {
            tmp,
            broker: Some(broker),
            handle: Some(handle),
            addr,
            key_file,
            key,
            next: 0,
        }
    }

    /// The fleet's pre-shared key, for a test that opens its own connection.
    #[must_use]
    pub fn key(&self) -> [u8; 32] {
        self.key
    }

    /// A god-cap client ON THE SEALED WIRE — the same transport the nodes use,
    /// so a test that could reach the broker at all is a test whose handshake
    /// succeeded.
    pub fn god(&self) -> aterm_link::transport::Conn {
        let (mut c, _closer) =
            aterm_link::transport::connect(&fleet_transport(self.key), &self.addr)
                .expect("connect to the broker");
        let cap = astream_cap::mint(SECRET, &format!("/f/{FLEET}/>")).expect("mint the god cap");
        c.attach(&cap.filter, &cap.tag).expect("attach the god cap");
        c
    }

    /// A cap file holding exactly `grants`, for a node id this fleet has not
    /// given a bridge — the rogue of §6.1, or an observer.
    pub fn cap_file(&self, name: &str, grants: &[String]) -> PathBuf {
        let path = self.tmp.join(format!("{name}.cap"));
        let caps: String = grants.iter().map(|g| cap_line(g)).collect();
        std::fs::write(&path, caps).expect("write a cap file");
        path
    }

    /// Boot one node: a headless `aterm-gui` that launches its own real
    /// `aterm-link serve` child, both pointed at this fleet's sealed broker.
    pub fn node(&mut self, tag: &str, accept_from: &[&str]) -> Node {
        self.node_with(tag, accept_from, &[], &[])
    }

    /// [`Fleet::node`] with extra `aterm-link serve` flags and extra environment
    /// on the `aterm-gui` the bridge is a child of.
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

    /// Run `aterm-link ls` against this fleet and answer its stdout.
    ///
    /// THE REAL BINARY, over the real sealed transport, with a read-only cap —
    /// which is the whole point of the verb: one `Last` round trip and no
    /// filesystem scan, so it works from a host that hosts nothing (§7).
    pub fn ls(&self) -> String {
        let cap = self.cap_file("ls", &[format!("ro:/f/{FLEET}/pub/>")]);
        let out = Command::new(env!("CARGO_BIN_EXE_aterm-link"))
            .args(["ls", "--fleet", FLEET, "--broker", &self.addr])
            .args(fleet_child_flags(&self.key_file))
            .args(["--cap-file", cap.to_string_lossy().as_ref()])
            .output()
            .expect("run aterm-link ls");
        assert!(
            out.status.success(),
            "aterm-link ls failed: {}",
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
    /// Extra environment for the `aterm-gui` this node runs — and therefore for
    /// the bridge child it launches. Applied AFTER
    /// [`strip_inherited_identity`], which deliberately removes
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
        if let Ok(pid) = std::fs::read_to_string(self.state.join("pid")) {
            if let Ok(pid) = pid.trim().parse::<i32>() {
                kill(pid, 9);
            }
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
        std::fs::create_dir_all(tmp.join("cfg/aterm")).expect("scratch config dir");
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
        assert!(
            ctl_sock.len() < MAX_SOCK_PATH,
            "socket path too long for sun_path: {ctl_sock}"
        );

        let cap_file = tmp.join("node.cap");
        let caps: String = node_grants(&node).iter().map(|g| cap_line(g)).collect();
        std::fs::write(&cap_file, caps).expect("write the node cap file");

        let mut fabric_cmd = format!(
            "{} serve --fleet {FLEET} --broker {} {} --cap-file {} --state {}",
            env!("CARGO_BIN_EXE_aterm-link"),
            fleet.addr,
            fleet_child_flags(&fleet.key_file).join(" "),
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
        me.gui = Some(me.spawn_gui());
        me.token = World::wait_for_token_at(&me.ctl_sock);
        me
    }

    fn spawn_gui(&self) -> Child {
        let out = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(&self.gui_log)
            .expect("gui log");
        let err = out.try_clone().expect("gui log clone");
        let mut cmd = Command::new(gui_binary());
        // BEFORE the explicit `.env` calls below, never after: `Command`
        // applies env operations in order, so a blanket removal that ran last
        // would delete the three variables this harness deliberately sets.
        strip_inherited_identity(&mut cmd);
        cmd.arg("--headless")
            .env("HOME", scratch_home(&self.tmp))
            .env("XDG_RUNTIME_DIR", self.tmp.join("run"))
            .env("XDG_CONFIG_HOME", self.tmp.join("cfg"))
            .env("SHELL", "/bin/sh")
            .env("ATERM_LINES", "40")
            .env("ATERM_COLUMNS", "120")
            .env("ATERM_FABRIC_COMMAND", &self.fabric_cmd)
            .stdin(Stdio::null())
            .stdout(out)
            .stderr(err);
        for (k, v) in &self.extra_env {
            cmd.env(k, v);
        }
        cmd.spawn().expect("launch aterm-gui --headless")
    }

    /// SIGKILL this node's aterm and bring a fresh one up on the same scratch
    /// world: the same broker, the same bridge state dir, the same node id.
    /// Every session it hosted is gone and its successor's are new — aterm mints
    /// a fresh sid and a fresh launch nonce at every launch, which is what makes
    /// this "a relaunch to a new epoch".
    pub fn relaunch(&mut self) {
        let _boot = boot_permit();
        if let Some(mut gui) = self.gui.take() {
            let _ = gui.kill();
            let _ = gui.wait();
        }
        if let Ok(pid) = std::fs::read_to_string(self.state.join("pid")) {
            if let Ok(pid) = pid.trim().parse::<i32>() {
                kill(pid, 9);
            }
        }
        let _ = std::fs::remove_file(self.state.join("pid"));
        self.gui = Some(self.spawn_gui());
        self.token = World::wait_for_token_at(&self.ctl_sock);
    }

    /// A fresh authenticated control connection.
    pub fn ctl(&self) -> Ctl {
        Ctl::connect(&self.ctl_sock, &self.token).expect("connect to aterm")
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
            let (rows, _) = fleet.god().last(&subject, "", 8).ok()?;
            rows.iter()
                .find(|(_, s, _)| *s == subject)
                .filter(|(_, _, b)| String::from_utf8_lossy(b).contains("state=live"))
                .map(|_| ())
        });
    }

    /// Every session aterm hosts, as `(local, sid, nonce)`.
    pub fn sessions(&self) -> Vec<(u64, String, String)> {
        self.verb("sessions")
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
        self.verb(&format!("@{sid} inbox --peek"))
            .rows()
            .iter()
            .filter(|r| r.starts_with("msg "))
            .cloned()
            .collect()
    }

    /// The session's LIVE generation, computed by the SAME function the bridge
    /// checks a `gen=` against — one source of truth, so a test that passes is
    /// not a test that agreed with itself.
    pub fn gen(&self, sid: &str) -> String {
        let reply = self.verb(&format!("@{sid} text --json"));
        let frame = reply.rows().first().expect("text --json answers one row");
        aterm_link::bridge::gen_of_frame(frame).expect("a frame carries a seq and rows")
    }

    /// Every `ev` record this node has published, newest last — over EVERY `ev`
    /// face it owns. See [`World::ev`].
    pub fn ev(&self, fleet: &Fleet) -> Vec<String> {
        let mut c = fleet.god();
        ev_rows(&mut c, &format!("/f/{FLEET}/pub/{}/>", self.node))
    }

    /// The tail of this node's aterm log, for a failure message.
    pub fn log_tail(&self) -> String {
        let body = std::fs::read_to_string(&self.gui_log).unwrap_or_default();
        let lines: Vec<&str> = body.lines().collect();
        lines[lines.len().saturating_sub(20)..].join("\n")
    }
}

/// The age reader must be able to read OUR OWN age.
///
/// This is the test that was missing. `refuse_a_dirty_machine` degrades to silence when
/// it cannot ask the question — correct behaviour, and precisely what hid a reader that
/// could never answer. So assert the reader directly against a process that certainly
/// exists: ourselves. If `ps` loses the `etime` keyword, or its format shifts, this fails
/// loudly instead of quietly disarming the guard.
#[test]
fn self_check_age_reader() {
    let me = std::process::id().to_string();
    let age = process_age_secs(&me).unwrap_or_else(|| {
        panic!("cannot read this process's own age; the dirty-machine guard is disarmed")
    });
    assert!(
        (0..86_400).contains(&age),
        "implausible age {age}s for the running test process"
    );
    assert_eq!(
        process_age_secs("0"),
        None,
        "pid 0 is not ours to see; the reader must answer None, not a bogus age"
    );
}

/// THE STALENESS GUARD MUST SEE EVERY CRATE IN THE BINARY, not one.
///
/// It used to walk `crates/aterm-gui/src` alone, so an edit to `crates/aterm-uds` or
/// `crates/aterm-types` — both compiled into aterm-gui, both audited by the round the
/// guard was written for — left it silent and the whole e2e suite drove the previous
/// binary. This drives [`newest_input`] against a SYNTHETIC tree: a fake binary, cargo's
/// depfile beside it, and the newest input in a crate that is not aterm-gui.
///
/// Deterministic: the mtimes are SET, not raced. No sleep, no clock comparison against
/// wall time.
#[test]
fn the_stale_guard_sees_every_crate_the_binary_was_built_from() {
    let dir = std::env::temp_dir().join(format!("atl-stale-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let uds = dir.join("crates/aterm-uds/src");
    let gui = dir.join("crates/aterm-gui/src");
    let git = dir.join(".git");
    let out = dir.join("target/debug/build/x/out");
    for d in [&uds, &gui, &git, &out] {
        std::fs::create_dir_all(d).expect("scratch tree");
    }
    let bin = dir.join("target/debug/aterm-gui");
    let spawnfd = uds.join("spawnfd.rs");
    let app = gui.join("app.rs");
    let index = git.join("index");
    let generated = out.join("join_table.rs");
    for f in [&bin, &spawnfd, &app, &index, &generated] {
        std::fs::write(f, b"x").expect("write");
    }

    // The binary is one hour old; aterm-gui's own source is two hours old; the aterm-uds
    // source, the git stamp and the generated file are all NEWER than the binary.
    let hour = Duration::from_secs(3600);
    let now = std::time::SystemTime::now();
    set_mtime(&bin, now - hour);
    set_mtime(&app, now - hour - hour);
    set_mtime(&spawnfd, now);
    set_mtime(&index, now);
    set_mtime(&generated, now);

    std::fs::write(
        bin.with_extension("d"),
        format!(
            "{}: {} {} {} {}\n{}:\n",
            bin.display(),
            app.display(),
            spawnfd.display(),
            index.display(),
            generated.display(),
            app.display(),
        ),
    )
    .expect("depfile");

    let (_, newest) = newest_input(&bin, "aterm-gui").expect("the depfile names inputs");
    assert_eq!(
        newest, spawnfd,
        "a source in a crate that is NOT aterm-gui must be able to make the binary \
         stale; `.git` stamps and OUT_DIR generated files must not be the answer"
    );

    // THE TARGET IS NOT AN INPUT, and neither stamp class is.
    let inputs = depfile_inputs(
        &std::fs::read_to_string(bin.with_extension("d")).expect("read"),
        &target_root(&bin),
    );
    assert_eq!(
        inputs,
        vec![app.clone(), spawnfd.clone()],
        "the rule's target, `.git/*` and anything under target/ are not sources"
    );

    // A path with an escaped space survives the split as ONE path.
    assert_eq!(
        depfile_inputs(
            "/a/bin: /a/one\\ two.rs /a/three.rs",
            std::path::Path::new("/a/target")
        ),
        vec![PathBuf::from("/a/one two.rs"), PathBuf::from("/a/three.rs")],
        "cargo escapes a space in a path as `\\ `"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// EVERY BINARY THIS CRATE FINDS IS AGE-CHECKED, not just the one somebody remembered.
///
/// `aterm-link` is its own workspace, so a binary of the aterm workspace cannot be
/// declared with `CARGO_BIN_EXE_` and has to be LOCATED — `target/<profile>/<name>`,
/// whatever the last build left there. There are two such finders: [`gui_binary`] here
/// and `ctl_binary` in `glance_and_tui.rs`. The staleness guard was wired into the first
/// only; the second carried the doctrine in its doc comment and none of it in its code,
/// and `aterm-ctl` was four and a half hours behind `control_verbs.rs` when that was
/// found — the file the round was auditing, with the suite green over it.
///
/// The check is STRUCTURAL: every `for profile in ["debug", "release"]` search in these
/// two files must call [`refuse_a_stale_binary`] on what it found. It covers the two
/// files it reads and no others, which is stated here rather than implied: a third
/// finder in a third file is not seen by this test, and adding one means adding it to
/// the list below.
#[test]
fn every_binary_this_crate_finds_is_age_checked() {
    for (name, src) in [
        ("harness/mod.rs", include_str!("mod.rs")),
        ("glance_and_tui.rs", include_str!("../glance_and_tui.rs")),
    ] {
        let mut searches = 0;
        for (i, _) in src.match_indices(r#"for profile in ["debug", "release"]"#) {
            searches += 1;
            let window = &src[i..src.len().min(i + 600)];
            assert!(
                window.contains("refuse_a_stale_binary"),
                "{name}: a binary is FOUND under target/<profile> and never age-checked. \
                 A found binary is whatever the last build left behind, and a suite that \
                 drives the previous one reports a defect in code that is not running:\n\
                 {window}"
            );
        }
        assert!(
            searches > 0,
            "{name}: the search this test guards has moved or been renamed, so this test \
             now guards nothing — re-point it rather than deleting it"
        );
    }
}

/// Set a file's modification time. `filetime` is not a dependency of this crate and one
/// would not be added for a test, so this is the `utimensat(2)` the crate would wrap.
fn set_mtime(path: &std::path::Path, when: std::time::SystemTime) {
    let secs = when
        .duration_since(std::time::UNIX_EPOCH)
        .expect("after the epoch")
        .as_secs();
    let status = Command::new("touch")
        .arg("-t")
        .arg(stamp(secs))
        .arg(path)
        .status()
        .expect("touch");
    assert!(status.success(), "touch {}", path.display());
}

/// `touch -t`'s `[[CC]YY]MMDDhhmm[.ss]`, computed from a Unix second so the test needs
/// no date library and no locale.
fn stamp(secs: u64) -> String {
    let out = Command::new("date")
        .args(["-r", &secs.to_string(), "+%Y%m%d%H%M.%S"])
        .output()
        .expect("date -r");
    assert!(out.status.success(), "date -r {secs}");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}
