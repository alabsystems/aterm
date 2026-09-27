// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 conformance for `InputUnreadGate` (aterm-spec's
//! `input_unread_gate_model`): the shipped `input_backlog::refuses`, fed by
//! the real sink's reading of a real pty, is the model's `DriverKey` guard at
//! every step — and the 2026-09-24 incident replays as the negative control.
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
//! * `a1` ← `min(b.wait / 500 ms, AgeMax)`, the probe's lower bound on the
//!   oldest unread byte's wait. A `Tick` sleeps to the NEXT 500 ms boundary of
//!   the oldest write's age plus a 100 ms margin (not a bare 550 ms sleep, so
//!   one slow step cannot land two ticks of age in one).
//!
//! TIMING IS BRACKETED, NOT HOPED FOR (the load-sensitive test audit of
//! 2026-09-27). The sleep puts every reading at or past its tick's floor, but
//! only the scheduler keeps it under the NEXT boundary: a test thread
//! descheduled for ~400 ms before a reading ages the byte into a tick the
//! model has not taken. So every reading with a byte queued is bracketed by
//! the instant before its oldest write and the instant after the reading
//! returned — the widest the wait can be — and a reading whose bracket
//! reaches the model's next boundary is [`Late`]: the scenario reruns on a
//! fresh pty rather than asserting on a projection the schedule, not the
//! code, decided. A reading inside its bracket is judged exactly as before,
//! so a mis-dated ledger or a moved `REFUSE_AFTER` still fails at once.
//!
//! `reading` and `stopped` are the model's side of the reader: `Freeze`
//! states that this test does not read, and `stopped` is `refuses`'s own
//! argument, checked in BOTH values at every step. `a2` is not projected —
//! the probe dates only the oldest byte.
//!
//! macOS-only, like the queue probes. The pairs come from `openpty`, whose
//! slave is inheritable; harmless here, because this test binary spawns no
//! child that could inherit it.
#![cfg(target_os = "macos")]

use std::collections::{BTreeMap, VecDeque};
use std::thread;
use std::time::{Duration, Instant};

use aterm_session::input_backlog::{InputBacklog, refuses};
use aterm_session::sink::{ImmediateWrite, SinkWriter};
use aterm_spec::derive::{Model, input_unread_gate_model};
use aterm_spec::interp;

type State = BTreeMap<&'static str, i64>;

/// One model tick of the oldest byte's wait — `Refuse = 2` ticks is
/// `REFUSE_AFTER`.
const TICK: Duration = Duration::from_millis(500);
/// How far past a tick boundary a `Tick` sleeps, so the probe's lower bound
/// has crossed it.
const MARGIN: Duration = Duration::from_millis(100);

/// A pty pair with the slave raw (`VMIN = 1`, `VTIME = 0`: a key-at-a-time
/// reader like every agent TUI) or canonical without echo, behind an
/// O_NONBLOCK master and the production-shaped sink.
struct Rig {
    master: i32,
    slave: i32,
    sink: SinkWriter,
    /// `(bytes, sent from, accepted by)` for every write the slave has not
    /// read. The ledger stamps a write between the two instants: a sleep
    /// measured from the second never under-ages it, and a reading's wait
    /// can be no longer than the time since the first.
    unread: VecDeque<(usize, Instant, Instant)>,
}

/// A reading taken too late to name the tick the model is in — the
/// scheduler's doing, proved by the bracket, never a verdict on the code.
struct Late {
    step: &'static str,
    /// The widest the oldest byte's wait could have been at the reading.
    bracket: Duration,
    /// The model's next tick boundary, which that reaches.
    boundary: Duration,
}

/// Scenario attempts. Only a reading whose own bracket proved it late
/// retries, and every real assertion panics on the attempt it fails. A late
/// attempt has itself spent a whole tick or more, so the next one starts in a
/// different scheduling episode without a sleep between them.
const ATTEMPTS: usize = 4;

/// Run `scenario` until one attempt takes every reading inside its tick.
fn conclusive(scenario: impl Fn() -> Result<(), Late>) {
    let mut late = Vec::new();
    for _ in 0..ATTEMPTS {
        match scenario() {
            Ok(()) => return,
            Err(Late {
                step,
                bracket,
                boundary,
            }) => late.push(format!("{step}: {bracket:?} >= {boundary:?}")),
        }
    }
    panic!(
        "every attempt took a reading past the model's next tick, so none \
         could be judged: {late:?}"
    );
}

impl Rig {
    fn new(raw: bool) -> Self {
        let (mut master, mut slave) = (-1, -1);
        // SAFETY: both out-params are live `c_int`s on this stack; the name,
        // termios and winsize pointers may be null.
        let rc = unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        assert_eq!(rc, 0, "openpty");
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

    /// Sleep until the oldest unread write's age crosses its next tick
    /// boundary (plus [`MARGIN`]) — one model `Tick`.
    fn tick(&self, ticks_so_far: i64) {
        let (_, _, at) = *self.unread.front().expect("a tick ages a queued byte");
        let ticks = u32::try_from(ticks_so_far + 1).expect("a small tick count");
        let due = at + TICK * ticks + MARGIN;
        let now = Instant::now();
        if due > now {
            thread::sleep(due - now);
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
        buf[..got].to_vec()
    }

    /// The sink's reading, cross-checked against what this test wrote.
    fn reading(&self) -> InputBacklog {
        let b = self.sink.input_backlog().expect("a pty master is measured");
        let bytes: usize = self.unread.iter().map(|(n, _, _)| n).sum();
        assert_eq!(
            b.queued, bytes,
            "the kernel counts every unread write: {b:?}"
        );
        assert_eq!(b.spilled, Some(0), "nothing spilled: {b:?}");
        b
    }

    /// [`Self::reading`], for the model state `st`: [`Late`] when the reading
    /// may have aged the oldest byte to the model's next tick, `a1 + 1`
    /// (there is none once `a1` is `AgeMax`, which the projection caps at).
    fn reading_in(&self, st: &State, step: &'static str) -> Result<InputBacklog, Late> {
        let b = self.reading();
        let returned = Instant::now();
        if let Some(&(_, sent, _)) = self.unread.front()
            && st["a1"] < 3
        {
            let bracket = returned - sent;
            let boundary = TICK * u32::try_from(st["a1"] + 1).expect("a small tick count");
            if bracket >= boundary {
                return Err(Late {
                    step,
                    bracket,
                    boundary,
                });
            }
        }
        Ok(b)
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
    let a1 = i64::try_from(b.wait.as_millis() / TICK.as_millis())
        .expect("small")
        .min(3);
    let raw = i64::from(!b.canonical);
    assert_eq!(
        (raw, q, a1),
        (st["raw"], st["q"], st["a1"]),
        "{step}: the real reading {b:?} does not project onto the model state {st:?}"
    );
}

/// The conformance check proper: whenever the queue has room, the shipped
/// `refuses` says NO exactly when the model's `DriverKey` is disabled — for a
/// running job and for a stopped one.
fn assert_gate_is_the_guard(model: &Model, b: &InputBacklog, st: &State, step: &str) {
    if st["q"] >= 2 {
        return; // `Cap`: the model's queue is full, whatever the gate says.
    }
    for stopped in [false, true] {
        let mut s = st.clone();
        s.insert("stopped", i64::from(stopped));
        assert_eq!(
            refuses(b, stopped),
            !model.action_enabled("DriverKey", &s),
            "{step} (stopped={stopped}): refuses({b:?}) disagrees with DriverKey at {s:?}"
        );
    }
}

/// Fire `action` on the model, and check the invariant after it.
fn fire(model: &Model, action: &str, st: &mut State) {
    assert!(model.fire(action, st), "{action} is disabled at {st:?}");
    assert!(
        model.check_invariant("NoDriverKeyBehindStaleInput", st),
        "{action}: {st:?}"
    );
}

/// THE STEP-BY-STEP BIND. A raw program that has stopped reading; the
/// human's Enter goes in and ages tick by tick. At every step the real
/// reading projects onto the model's state, and `refuses` is `DriverKey`'s
/// guard: open at 0 and 500 ms, shut from 1 s. A second human key still
/// lands while the gate refuses (the window keyboard is never gated) and
/// the kernel's count grows. The program reading everything clears it, and
/// the gate opens again.
#[test]
fn refuses_is_the_models_driver_key_guard_at_every_step() {
    conclusive(|| {
        let model = input_unread_gate_model();
        let mut rig = Rig::new(true);
        let mut st = model.init_state();

        fire(&model, "Freeze", &mut st);
        let b = rig.reading_in(&st, "empty")?;
        assert_projects(&rig, &b, &st, "empty");
        assert_gate_is_the_guard(&model, &b, &st, "empty");

        rig.write(b"\r");
        fire(&model, "HumanKey", &mut st);
        for ticks in 0..2 {
            let b = rig.reading_in(&st, "aging")?;
            assert_projects(&rig, &b, &st, "aging");
            assert_gate_is_the_guard(&model, &b, &st, "aging");
            assert!(!refuses(&b, false), "younger than REFUSE_AFTER: {b:?}");
            rig.tick(ticks);
            fire(&model, "Tick", &mut st);
        }
        let stale = rig.reading_in(&st, "stale")?;
        assert_projects(&rig, &stale, &st, "stale");
        assert_gate_is_the_guard(&model, &stale, &st, "stale");
        assert!(refuses(&stale, false), "past REFUSE_AFTER: {stale:?}");

        // The human types again while the gate refuses: it lands, and it
        // grows the count the gate reads.
        assert!(model.action_enabled("HumanKey", &st));
        rig.write(b"\r");
        fire(&model, "HumanKey", &mut st);
        let grown = rig.reading_in(&st, "grown")?;
        assert_eq!(grown.queued, 2);
        assert_projects(&rig, &grown, &st, "grown");
        assert!(refuses(&grown, false), "{grown:?}");

        // The program wakes and reads both keys.
        assert_eq!(rig.read_all(), b"\r\r");
        fire(&model, "Thaw", &mut st);
        fire(&model, "Consume", &mut st);
        fire(&model, "Consume", &mut st);
        let cleared = rig.reading_in(&st, "cleared")?;
        assert_projects(&rig, &cleared, &st, "cleared");
        assert_gate_is_the_guard(&model, &cleared, &st, "cleared");
        assert!(!refuses(&cleared, false));
        Ok(())
    });
}

/// A driver's key into an EMPTY queue is admitted — and so is a second one
/// straight behind it, which is younger than `REFUSE_AFTER`. The gate stops
/// a key only behind input the program has left unread for a second.
#[test]
fn a_driver_key_into_a_fresh_queue_is_admitted() {
    conclusive(|| {
        let model = input_unread_gate_model();
        let mut rig = Rig::new(true);
        let mut st = model.init_state();
        fire(&model, "Freeze", &mut st);

        for key in [&b"\x1b[B"[..], b"\r"] {
            let b = rig.reading_in(&st, "driver")?;
            assert_projects(&rig, &b, &st, "driver");
            assert_gate_is_the_guard(&model, &b, &st, "driver");
            assert!(!refuses(&b, false), "{b:?}");
            rig.write(key);
            fire(&model, "DriverKey", &mut st);
        }
        assert_eq!(rig.read_all(), b"\x1b[B\r");
        Ok(())
    });
}

/// CANONICAL TYPE-AHEAD NEVER REFUSES. A shell's complete line `ls\n` that
/// nothing has read yet ages past `REFUSE_AFTER` and the gate stays open —
/// the model's `raw = 0` branch.
#[test]
fn aged_canonical_typeahead_is_admitted() {
    conclusive(|| {
        let model = input_unread_gate_model();
        let mut rig = Rig::new(false);
        let mut st = model.init_state();
        fire(&model, "ModeFlip", &mut st);
        fire(&model, "Freeze", &mut st);

        rig.write(b"ls\n");
        fire(&model, "HumanKey", &mut st);
        for ticks in 0..2 {
            rig.tick(ticks);
            fire(&model, "Tick", &mut st);
        }
        let b = rig.reading_in(&st, "typeahead")?;
        assert!(b.canonical && b.wait >= Duration::from_secs(1), "{b:?}");
        assert_projects(&rig, &b, &st, "typeahead");
        assert_gate_is_the_guard(&model, &b, &st, "typeahead");
        assert!(!refuses(&b, false), "{b:?}");
        assert_eq!(rig.read_all(), b"ls\n");
        Ok(())
    });
}

/// The incident's schedule on both sides: the program stops reading, the
/// human's Enter goes in, and it ages two ticks — past `REFUSE_AFTER`, to
/// 1.1 s.
fn enter_and_age_past_refuse(rig: &mut Rig, model: &Model, st: &mut State) {
    fire(model, "Freeze", st);
    rig.write(b"\r");
    fire(model, "HumanKey", st);
    for ticks in 0..2 {
        rig.tick(ticks);
        fire(model, "Tick", st);
    }
}

/// NEGATIVE CONTROL — THE INCIDENT, REPLAYED. The human's Enter goes into a
/// raw program that is not reading, and waits 1.1 s.
///
/// Today's actuator, `try_write_frame_immediate`, refuses only on lock
/// contention, a non-empty spill or a full queue, so the driver's `down`
/// answers `Full` and the program's next `read()` returns `\r\x1b[B` — both
/// keys together, the down acted on against a screen it never drew. The
/// model's screen-only fence (`Buggy = 1`) admits `DriverKey` at the same
/// projected state, and firing it falsifies `NoDriverKeyBehindStaleInput`.
///
/// Through `refuses` the same reading withholds the key, the correct model
/// agrees `DriverKey` is disabled, and the program reads exactly `\r`.
#[test]
fn the_incident_replays_and_the_gate_withholds_the_key() {
    conclusive(|| {
        let model = input_unread_gate_model();
        let fence = interp::with_buggy(&model, 1);

        // Today: the screen-only fence.
        let mut rig = Rig::new(true);
        let mut st = fence.init_state();
        enter_and_age_past_refuse(&mut rig, &fence, &mut st);
        let b = rig.reading_in(&st, "incident")?;
        assert!(b.wait >= Duration::from_millis(1100), "{b:?}");
        assert_projects(&rig, &b, &st, "incident");
        assert!(fence.action_enabled("DriverKey", &st));
        assert!(fence.fire("DriverKey", &mut st));
        assert!(
            !fence.check_invariant("NoDriverKeyBehindStaleInput", &st),
            "the screen-only fence lands the key behind the stale Enter"
        );
        assert_eq!(
            rig.sink.try_write_frame_immediate(b"\x1b[B"),
            ImmediateWrite::Full,
            "today's actuator writes behind the unread Enter"
        );
        let now = Instant::now();
        rig.unread.push_back((3, now, now));
        assert_eq!(
            rig.read_all(),
            b"\r\x1b[B",
            "one read() hands the program both keys — the incident"
        );
        drop(rig);

        // With the gate: the same schedule, the same reading, the key withheld.
        let mut rig = Rig::new(true);
        let mut st = model.init_state();
        enter_and_age_past_refuse(&mut rig, &model, &mut st);
        let b = rig.reading_in(&st, "gated")?;
        assert!(b.wait >= Duration::from_millis(1100), "{b:?}");
        assert_projects(&rig, &b, &st, "gated");
        assert_gate_is_the_guard(&model, &b, &st, "gated");
        assert!(refuses(&b, false), "the gate withholds the key: {b:?}");
        assert!(!model.fire("DriverKey", &mut st));
        assert_eq!(rig.read_all(), b"\r", "the program reads exactly its Enter");
        assert_eq!(rig.reading().queued, 0);
        Ok(())
    });
}
