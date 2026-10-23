// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance for `InputUnreadGate` (aterm-spec's `input_unread_gate_model`).
#![cfg(target_os = "macos")]

use std::collections::{BTreeMap, VecDeque};
use std::time::{Duration, Instant};

use aterm_models_session::input_unread_gate_model;
use aterm_session::input_backlog::{
    Contention, Gate, InputBacklog, InputWord, Liveness, STALL_AFTER, WARN_AFTER, classify, gate,
};
use aterm_session::sink::{ImmediateWrite, SinkWriter};
use aterm_spec::derive::Model;
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
    /// read.
    unread: VecDeque<(usize, Instant, Instant)>,
    /// How old the model says the oldest unread write is: what a [`Self::tick`]
    /// advances, and what [`Self::reading`]'s clock adds to the write.
    age: Duration,
}

impl Rig {
    fn new(raw: bool) -> Self {
        let (master, slave) = aterm_pty::open_pty_pair().expect("open_pty_pair");
        // SAFETY: `libc::termios` is plain integer fields and a byte array; all zeros is a valid.
        let mut t: libc::termios = unsafe { std::mem::zeroed() };
        // SAFETY: `slave` is this test's live pty slave; `t` is a valid out-param.
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
    /// model `Tick`.
    fn tick(&mut self, a1: i64) {
        assert!(!self.unread.is_empty(), "a tick ages a queued byte");
        self.age = boundary(a1).expect("a band that ends");
    }

    /// The instant a reading is taken as of.
    fn now(&self) -> Instant {
        match (self.unread.front(), self.unread.back()) {
            (Some(&(_, sent, _)), _) if self.age.is_zero() => sent,
            (_, Some(&(_, _, accepted))) => accepted + self.age,
            _ => Instant::now(),
        }
    }

    /// The program reads everything it has been sent, in one `read()`, and returns it.
    fn read_all(&mut self) -> Vec<u8> {
        let want: usize = self.unread.iter().map(|(n, _, _)| n).sum();
        let mut buf = [0u8; 64];
        // SAFETY: a bounded read into this stack buffer from this test's live slave; the bytes.
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
        // SAFETY: both fds are this rig's and still open.
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
            // `Cap`: the model's queue is full, whatever the gate says.
            continue;
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

/// A raw program that stopped reading; the human's Enter ages band by band, and at
/// every step the reading projects onto the model and `gate`/`classify` are its
/// guards: open under a second, a warning from a second on (the key is written),
/// still a warning at ten seconds until the watch publishes the stall, then a
/// refusal.
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

    assert!(model.action_enabled("HumanKey", &st));
    rig.write(b"\r");
    fire(&model, "HumanKey", &mut st);
    let grown = step(&rig, &model, &st, "grown");
    assert_eq!(grown.queued, 2);
    assert_eq!(gate(&grown, false, true), Gate::Refuse, "{grown:?}");

    assert_eq!(rig.read_all(), b"\r\r");
    fire(&model, "Thaw", &mut st);
    fire(&model, "Consume", &mut st);
    fire(&model, "Consume", &mut st);
    let cleared = step(&rig, &model, &st, "cleared");
    assert_eq!(gate(&cleared, false, true), Gate::Open);
    fire(&model, "Withdraw", &mut st);
    step(&rig, &model, &st, "withdrawn");
}

/// A driver's key into an empty queue is written, and so is a second one straight
/// behind it, younger than `WARN_AFTER`: no warning either.
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

/// Canonical type-ahead is neither warned nor refused: a complete line `ls\n` nothing
/// has read ages past `WARN_AFTER` and the gate stays open (the model's `raw = 0`
/// branch).
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

/// Negative control, the static cap: an Enter unread for 1.1 s (on a loaded machine,
/// a healthy program) gets a warning from the shipped `gate`, and the driver's key is
/// written; the program reads both in order.
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

/// Negative control, a screen-only fence: the human's Enter goes into a raw program
/// that is not reading, ages past `STALL_AFTER`, and the watch publishes the stall.
#[test]
fn the_incident_replays_and_the_gate_withholds_the_key() {
    let model = input_unread_gate_model();
    let fence = interp::with_buggy(&model, 1);
    let mut unguarded = Rig::new(true);
    let mut rig = Rig::new(true);
    let mut st = model.init_state();
    enter_and_age_to_stall(&mut [&mut unguarded, &mut rig], &model, &mut st);
    fire(&model, "Publish", &mut st);

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

    let b = step(&rig, &model, &st, "gated");
    assert!(b.wait >= STALL_AFTER, "{b:?}");
    assert_eq!(gate(&b, false, true), Gate::Refuse, "withheld: {b:?}");
    assert!(!model.fire("DriverKey", &mut st));
    assert_eq!(rig.read_all(), b"\r", "the program reads exactly its Enter");
    assert_eq!(rig.reading().queued, 0);
}
