// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance for `InputUnreadGate` (aterm-spec's
//! `input_unread_gate_model`): the shipped `input_backlog::gate`, fed by the
//! real sink's reading of a real pty, is the model's `DriverKey` /
//! `DriverRefused` guard and its warning at every step, and `classify`'s
//! entry is its `Publish` guard — and both the frozen-program incident and a
//! static 1 s refusal replay as negative controls.
//!
//! The incident: a frozen Claude Code left the owner's Enter unread in its
//! slave's input queue; a supervisor, fencing its keys on a screen the frozen
//! program never redrew, wrote a `down` into the same queue; and one `read()`
//! later handed the program `\r\x1b[B` together. Here the "program" is a raw
//! slave this test simply does not read — which is all a frozen program is,
//! from the terminal's side.
//!
//! PROJECTION (real → model), at every step:
//!
//! * `raw` ← `!b.canonical`, the slave's own termios;
//! * `q` ← the number of writes this test made that the slave has not read,
//!   cross-checked against the kernel's `b.queued` (their byte sum) and a
//!   drained spill;
//! * `a1` ← the BAND of `b.wait`, the probe's lower bound on the oldest
//!   unread byte's wait: `0` under `WARN_AFTER`, `1` under `STALL_AFTER`,
//!   `2` from there. A `Tick` moves the reading's clock to the NEXT band
//!   boundary of the oldest write's age;
//! * `published` ← the model's own, handed to `gate` as the watch's
//!   publication — the watch itself is aterm-gui's (`input_stall`), bound by
//!   its own tests; what binds here is that the rule the watch's evidence
//!   feeds (`classify`) and the rule the publication feeds (`gate`) are the
//!   model's guards.
//!
//! THE CLOCK IS INJECTED, NOT SLEPT ON. The writes are real — a real pty, the
//! real sink and ledger, the kernel's own queue count — but the instant a
//! reading is taken AS OF is handed to `SinkWriter::input_backlog_at`: the
//! writes' own bracketing instants plus the model's band boundary, so the
//! reading is at or past its band's floor and under the next boundary
//! whatever the scheduler does. Nothing sleeps, nothing is retried, and a
//! mis-dated ledger or a moved `WARN_AFTER`/`STALL_AFTER` still fails at
//! once. (The real kernel queue dating, wall clock and all, is
//! `input_backlog_pty.rs`'s.)
//!
//! `reading` and `stopped` are the model's side of the reader: `Freeze`
//! states that this test does not read, and `stopped` is `gate`'s own
//! argument, checked in BOTH values at every step. `a2` is not projected —
//! the probe dates only the oldest byte.
//!
//! macOS-only, like the queue probes. The pairs come from aterm-pty's
//! `open_pty_pair`, both ends close-on-exec from birth, as every test's pair
//! does: `openpty(3)`'s are inheritable, and a child spawned meanwhile keeps
//! them.
#![cfg(target_os = "macos")]

use std::collections::{BTreeMap, VecDeque};
use std::time::{Duration, Instant};

use aterm_session::input_backlog::{
    Contention, Gate, InputBacklog, InputWord, Liveness, STALL_AFTER, WARN_AFTER, classify, gate,
};
use aterm_session::sink::{ImmediateWrite, SinkWriter};
use aterm_spec::derive::{Model, input_unread_gate_model};
use aterm_spec::interp;

type State = BTreeMap<&'static str, i64>;

/// The model's band boundaries: band `i` ends at `BANDS[i]` — `Warn = 1`
/// is `WARN_AFTER`, `Stall = 2` is `STALL_AFTER`.
const BANDS: [Duration; 2] = [WARN_AFTER, STALL_AFTER];
/// The model's band for a wait.
fn band(wait: Duration) -> i64 {
    i64::try_from(BANDS.iter().filter(|b| wait >= **b).count()).expect("small")
}

/// The boundary that ends band `a1`, or `None` for the last band.
fn boundary(a1: i64) -> Option<Duration> {
    usize::try_from(a1).ok().and_then(|i| BANDS.get(i)).copied()
}

/// A pty pair with the slave raw (`VMIN = 1`, `VTIME = 0`: a key-at-a-time
/// reader like every agent TUI) or canonical without echo, behind an
/// O_NONBLOCK master and the production-shaped sink.
struct Rig {
    master: i32,
    slave: i32,
    sink: SinkWriter,
    /// `(bytes, sent from, accepted by)` for every write the slave has not
    /// read. The ledger stamps a write between the two instants ([`Self::now`]
    /// reads the clock off them).
    unread: VecDeque<(usize, Instant, Instant)>,
    /// How old the model says the oldest unread write is: what a [`Self::tick`]
    /// advances, and what [`Self::reading`]'s clock adds to the write.
    age: Duration,
}

impl Rig {
    fn new(raw: bool) -> Self {
        let (master, slave) = aterm_pty::open_pty_pair().expect("open_pty_pair");
        // SAFETY: `libc::termios` is plain integer fields and a byte array;
        // all zeros is a valid value, overwritten by `tcgetattr` below.
        let mut t: libc::termios = unsafe { std::mem::zeroed() };
        // SAFETY: `slave` is this test's live pty slave; `t` is a valid
        // out-param.
        assert_eq!(unsafe { libc::tcgetattr(slave, &mut t) }, 0);
        if raw {
            // SAFETY: `t` is a live, initialised termios on this stack.
            unsafe { libc::cfmakeraw(&mut t) };
            t.c_cc[libc::VMIN] = 1;
            t.c_cc[libc::VTIME] = 0;
        } else {
            t.c_lflag |= libc::ICANON;
            t.c_lflag &= !libc::ECHO;
        }
        // SAFETY: `slave` is live; `t` is the termios just derived from its own.
        assert_eq!(unsafe { libc::tcsetattr(slave, libc::TCSANOW, &t) }, 0);
        // SAFETY: `master` is this test's live fd; F_GETFL/F_SETFL take ints.
        let flags = unsafe { libc::fcntl(master, libc::F_GETFL) };
        assert!(flags >= 0);
        // SAFETY: as above.
        assert_eq!(
            unsafe { libc::fcntl(master, libc::F_SETFL, flags | libc::O_NONBLOCK) },
            0
        );
        let sink = SinkWriter::new(master);
        sink.note_master_nonblocking(true);
        Self {
            master,
            slave,
            sink,
            unread: VecDeque::new(),
            age: Duration::ZERO,
        }
    }

    /// A write that must land whole in the kernel's queue — the human's
    /// window keyboard (never gated), or a driver key the gate admitted.
    fn write(&mut self, bytes: &[u8]) {
        let sent = Instant::now();
        assert_eq!(
            self.sink.write_frame_nonparking(bytes).expect("write"),
            bytes.len()
        );
        self.unread.push_back((bytes.len(), sent, Instant::now()));
    }

    /// Age the oldest unread write to the boundary that ends band `a1` — one
    /// model `Tick`. The clock moves; nothing sleeps.
    fn tick(&mut self, a1: i64) {
        assert!(!self.unread.is_empty(), "a tick ages a queued byte");
        self.age = boundary(a1).expect("a band that ends");
    }

    /// The instant a reading is taken as of. At age zero it is the instant
    /// BEFORE the oldest unread write, ahead of every stamp the ledger can
    /// have taken (a wait of exactly zero). Past zero it is the instant the
    /// NEWEST unread write was accepted plus the [`Self::age`]: the ledger
    /// dates a byte by one write's stamp, and coalescing dates a run by its
    /// newest, so no stamp is later than that acceptance and the wait is at
    /// least the age — and at most the age plus the span the test's writes
    /// took, microseconds. With nothing queued there is no wait to date.
    fn now(&self) -> Instant {
        match (self.unread.front(), self.unread.back()) {
            (Some(&(_, sent, _)), _) if self.age.is_zero() => sent,
            (_, Some(&(_, _, accepted))) => accepted + self.age,
            _ => Instant::now(),
        }
    }

    /// The program reads everything it has been sent, in one `read()` — the
    /// shape the incident's reader took. Returns what it read.
    fn read_all(&mut self) -> Vec<u8> {
        let want: usize = self.unread.iter().map(|(n, _, _)| n).sum();
        let mut buf = [0u8; 64];
        // SAFETY: a bounded read into this stack buffer from this test's
        // live slave; the bytes are already queued, so it does not block.
        let r = unsafe { libc::read(self.slave, buf.as_mut_ptr().cast(), buf.len()) };
        let got = usize::try_from(r).expect("read");
        assert_eq!(got, want, "one read() returns every queued byte");
        self.unread.clear();
        self.age = Duration::ZERO;
        buf[..got].to_vec()
    }

    /// The sink's reading, cross-checked against what this test wrote.
    fn reading(&self) -> InputBacklog {
        let b = self
            .sink
            .input_backlog_at(self.now())
            .expect("a pty master is measured");
        let bytes: usize = self.unread.iter().map(|(n, _, _)| n).sum();
        assert_eq!(
            b.queued, bytes,
            "the kernel counts every unread write: {b:?}"
        );
        assert_eq!(b.spilled, Some(0), "nothing spilled: {b:?}");
        b
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        // SAFETY: both fds are this rig's and still open. The sink shares
        // only the master's NUMBER; nothing writes through it after this.
        unsafe {
            libc::close(self.slave);
            libc::close(self.master);
        }
    }
}

/// Project the reading onto the model's `raw`/`q`/`a1` and assert they are
/// the model's own values in `st`.
fn assert_projects(rig: &Rig, b: &InputBacklog, st: &State, step: &str) {
    let q = i64::try_from(rig.unread.len()).expect("small");
    let raw = i64::from(!b.canonical);
    assert_eq!(
        (raw, q, band(b.wait)),
        (st["raw"], st["q"], st["a1"]),
        "{step}: the real reading {b:?} does not project onto the model state {st:?}"
    );
}

/// The conformance check proper, for a running job and a stopped one:
///
/// * `gate` REFUSES exactly when the model's `DriverRefused` is enabled — and,
///   whenever the queue has room, exactly when `DriverKey` is disabled;
/// * `gate` WARNS exactly when a `DriverKey` fired from this state leaves
///   `warned = 1`;
/// * `classify`, fed the watch's evidence for the model's reader (a frozen
///   reader spins and draws nothing; a reading one is seen drawing), enters
///   `stalled` exactly when `Publish` is enabled.
fn assert_gate_is_the_guard(model: &Model, b: &InputBacklog, st: &State, step: &str) {
    let published = st["published"] == 1;
    for stopped in [false, true] {
        let mut s = st.clone();
        s.insert("stopped", i64::from(stopped));
        let g = gate(b, stopped, published);
        assert_eq!(
            g == Gate::Refuse,
            model.action_enabled("DriverRefused", &s),
            "{step} (stopped={stopped}): gate({b:?}) = {g:?} disagrees with DriverRefused at {s:?}"
        );
        if s["q"] >= 2 {
            continue; // `Cap`: the model's queue is full, whatever the gate says.
        }
        assert_eq!(
            g == Gate::Refuse,
            !model.action_enabled("DriverKey", &s),
            "{step} (stopped={stopped}): gate({b:?}) = {g:?} disagrees with DriverKey at {s:?}"
        );
        if g != Gate::Refuse {
            let mut written = s.clone();
            assert!(model.fire("DriverKey", &mut written));
            assert_eq!(
                g == Gate::Warn,
                written["warned"] == 1,
                "{step} (stopped={stopped}): gate({b:?}) = {g:?} disagrees with the warning"
            );
        }
        if !stopped && !published {
            let frozen = st["reading"] == 0;
            let seen = Liveness::Watched {
                drew: !frozen,
                spinning: frozen,
                contention: Contention::default(),
            };
            assert_eq!(
                classify(Some(b), false, seen) == InputWord::Stalled,
                model.action_enabled("Publish", &s),
                "{step}: classify({b:?}, {seen:?}) disagrees with Publish at {s:?}"
            );
        }
    }
}

/// Fire `action` on the model, and check every invariant after it.
fn fire(model: &Model, action: &str, st: &mut State) {
    assert!(model.fire(action, st), "{action} is disabled at {st:?}");
    for inv in [
        "NoDriverKeyBehindStalledInput",
        "NoRefusalWithoutAnEstablishedStall",
        "NoReadingProgramPublished",
    ] {
        assert!(model.check_invariant(inv, st), "{action}: {inv} at {st:?}");
    }
}

/// Read, project, and bind one step.
fn step(rig: &Rig, model: &Model, st: &State, name: &'static str) -> InputBacklog {
    let b = rig.reading();
    assert_projects(rig, &b, st, name);
    assert_gate_is_the_guard(model, &b, st, name);
    b
}

/// THE STEP-BY-STEP BIND. A raw program that has stopped reading; the
/// human's Enter goes in and ages band by band. At every step the real
/// reading projects onto the model's state and `gate`/`classify` are its
/// guards: open under a second, a WARNING from a second on — the key would be
/// written — and still only a warning at ten seconds, until the watch's
/// evidence publishes the stall; only then a refusal. A second human key
/// lands while the gate refuses (the window keyboard is never gated). The
/// program reading everything opens the gate, and the watch withdraws.
#[test]
fn gate_and_classify_are_the_models_guards_at_every_step() {
    let model = input_unread_gate_model();
    let mut rig = Rig::new(true);
    let mut st = model.init_state();

    fire(&model, "Freeze", &mut st);
    step(&rig, &model, &st, "empty");

    rig.write(b"\r");
    fire(&model, "HumanKey", &mut st);
    let b = step(&rig, &model, &st, "young");
    assert_eq!(gate(&b, false, false), Gate::Open, "{b:?}");
    rig.tick(0);
    fire(&model, "Tick", &mut st);
    let b = step(&rig, &model, &st, "warn band");
    assert_eq!(gate(&b, false, false), Gate::Warn, "{b:?}");
    rig.tick(1);
    fire(&model, "Tick", &mut st);
    let b = step(&rig, &model, &st, "stall band");
    assert_eq!(
        gate(&b, false, false),
        Gate::Warn,
        "ten seconds unread is still only a warning until the watch publishes: {b:?}"
    );

    fire(&model, "Publish", &mut st);
    let stale = step(&rig, &model, &st, "published");
    assert_eq!(gate(&stale, false, true), Gate::Refuse, "{stale:?}");

    // The human types again while the gate refuses: it lands, and it
    // grows the count the gate reads.
    assert!(model.action_enabled("HumanKey", &st));
    rig.write(b"\r");
    fire(&model, "HumanKey", &mut st);
    let grown = step(&rig, &model, &st, "grown");
    assert_eq!(grown.queued, 2);
    assert_eq!(gate(&grown, false, true), Gate::Refuse, "{grown:?}");

    // The program wakes and reads both keys; the gate opens and the watch
    // withdraws.
    assert_eq!(rig.read_all(), b"\r\r");
    fire(&model, "Thaw", &mut st);
    fire(&model, "Consume", &mut st);
    fire(&model, "Consume", &mut st);
    let cleared = step(&rig, &model, &st, "cleared");
    assert_eq!(gate(&cleared, false, true), Gate::Open);
    fire(&model, "Withdraw", &mut st);
    step(&rig, &model, &st, "withdrawn");
}

/// A driver's key into an EMPTY queue is written — and so is a second one
/// straight behind it, which is younger than `WARN_AFTER`: no warning either.
#[test]
fn a_driver_key_into_a_fresh_queue_is_written_without_a_warning() {
    let model = input_unread_gate_model();
    let mut rig = Rig::new(true);
    let mut st = model.init_state();
    fire(&model, "Freeze", &mut st);

    for key in [&b"\x1b[B"[..], b"\r"] {
        let b = step(&rig, &model, &st, "driver");
        assert_eq!(gate(&b, false, false), Gate::Open, "{b:?}");
        rig.write(key);
        fire(&model, "DriverKey", &mut st);
        assert_eq!(st["warned"], 0);
    }
    assert_eq!(rig.read_all(), b"\x1b[B\r");
}

/// CANONICAL TYPE-AHEAD IS NEITHER WARNED NOR REFUSED. A shell's complete
/// line `ls\n` that nothing has read yet ages past `WARN_AFTER` and the gate
/// stays open — the model's `raw = 0` branch.
#[test]
fn aged_canonical_typeahead_is_written_without_a_warning() {
    let model = input_unread_gate_model();
    let mut rig = Rig::new(false);
    let mut st = model.init_state();
    fire(&model, "ModeFlip", &mut st);
    fire(&model, "Freeze", &mut st);

    rig.write(b"ls\n");
    fire(&model, "HumanKey", &mut st);
    rig.tick(0);
    fire(&model, "Tick", &mut st);
    let b = step(&rig, &model, &st, "typeahead");
    assert!(b.canonical && b.wait >= WARN_AFTER, "{b:?}");
    assert_eq!(gate(&b, false, false), Gate::Open, "{b:?}");
    assert_eq!(rig.read_all(), b"ls\n");
}

/// NEGATIVE CONTROL — THE STATIC CAP. The
/// human's Enter has waited 1.1 s in a program that has not read it yet — on
/// a machine at a load of 150 that is a healthy Claude Code. The shipped
/// `gate` WARNS, and the driver's key is written: the program reads both, in
/// order. The static cap (the model's mutant family) refuses the same key at
/// the same projected state with nothing established, and firing its
/// refusal falsifies `NoRefusalWithoutAnEstablishedStall`.
#[test]
fn a_slow_read_is_warned_and_written_and_the_static_cap_is_caught() {
    let model = input_unread_gate_model();
    let cap = interp::with_buggy(&model, 1);
    let mut rig = Rig::new(true);
    let mut st = model.init_state();
    rig.write(b"\r");
    fire(&model, "HumanKey", &mut st);
    rig.tick(0);
    fire(&model, "Tick", &mut st);
    let b = step(&rig, &model, &st, "slow");
    assert_eq!(gate(&b, false, false), Gate::Warn, "{b:?}");

    let mut capped = st.clone();
    assert!(cap.action_enabled("DriverRefused", &capped));
    assert!(cap.fire("DriverRefused", &mut capped));
    assert!(
        !cap.check_invariant("NoRefusalWithoutAnEstablishedStall", &capped),
        "the static cap refuses a program nothing has shown is stuck"
    );

    rig.write(b"\x1b[B");
    fire(&model, "DriverKey", &mut st);
    assert_eq!(st["warned"], 1, "the reply carries the warning");
    assert_eq!(rig.read_all(), b"\r\x1b[B", "read in order, both");
}

/// Age a fresh Enter into the stall band on every rig at once, and fire the
/// same schedule on `model` — so two rigs share one ten-second age.
fn enter_and_age_to_stall(rigs: &mut [&mut Rig], model: &Model, st: &mut State) {
    fire(model, "Freeze", st);
    for rig in rigs.iter_mut() {
        rig.write(b"\r");
    }
    fire(model, "HumanKey", st);
    for a1 in 0..2 {
        for rig in rigs.iter_mut() {
            rig.tick(a1);
        }
        fire(model, "Tick", st);
    }
}

/// NEGATIVE CONTROL — THE INCIDENT, REPLAYED. The human's Enter goes into a
/// raw program that is not reading, ages past `STALL_AFTER`, and the watch's
/// evidence publishes the stall.
///
/// The raw actuator, `try_write_frame_immediate`, refuses only on lock
/// contention, a non-empty spill or a full queue, so the driver's `down`
/// answers `Full` and the program's next `read()` returns `\r\x1b[B` — both
/// keys together, the down acted on against a screen it never drew. The
/// model's screen-only fence (`Buggy = 1`) admits `DriverKey` at the same
/// projected state, and firing it falsifies `NoDriverKeyBehindStalledInput`.
///
/// Through `gate` the same reading withholds the key, the correct model
/// agrees `DriverKey` is disabled, and the program reads exactly `\r`.
#[test]
fn the_incident_replays_and_the_gate_withholds_the_key() {
    let model = input_unread_gate_model();
    let fence = interp::with_buggy(&model, 1);
    let mut unguarded = Rig::new(true);
    let mut rig = Rig::new(true);
    let mut st = model.init_state();
    enter_and_age_to_stall(&mut [&mut unguarded, &mut rig], &model, &mut st);
    fire(&model, "Publish", &mut st);

    // The screen-only fence.
    let b = unguarded.reading();
    assert!(b.wait >= STALL_AFTER, "{b:?}");
    assert_projects(&unguarded, &b, &st, "incident");
    let mut screen = st.clone();
    assert!(fence.action_enabled("DriverKey", &screen));
    assert!(fence.fire("DriverKey", &mut screen));
    assert!(
        !fence.check_invariant("NoDriverKeyBehindStalledInput", &screen),
        "the screen-only fence lands the key behind the stale Enter"
    );
    assert_eq!(
        unguarded.sink.try_write_frame_immediate(b"\x1b[B"),
        ImmediateWrite::Full,
        "the raw actuator writes behind the unread Enter"
    );
    let now = Instant::now();
    unguarded.unread.push_back((3, now, now));
    assert_eq!(
        unguarded.read_all(),
        b"\r\x1b[B",
        "one read() hands the program both keys — the incident"
    );

    // With the gate: the same schedule, the same reading, the key withheld.
    let b = step(&rig, &model, &st, "gated");
    assert!(b.wait >= STALL_AFTER, "{b:?}");
    assert_eq!(gate(&b, false, true), Gate::Refuse, "withheld: {b:?}");
    assert!(!model.fire("DriverKey", &mut st));
    assert_eq!(rig.read_all(), b"\r", "the program reads exactly its Enter");
    assert_eq!(rig.reading().queued, 0);
}
