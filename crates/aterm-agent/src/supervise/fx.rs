// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `Fx`: the ONE DOOR the supervising loop reads its clocks, sleeps, files
//! and flags through, and the one a harness tape records it at.
//!
//! Every value the loop thread observes from outside itself is a value a
//! harness tape will have to hold (the replay plan's invariant: a tape is every
//! such value, in the order the loop observed it), and the cheapest, most
//! frequent of them are the clocks: the monotonic one its waits, settles and
//! back-offs run on, and the wall one a limit notice's reset, a journal row and
//! a Mac's sleep are read on. Before this door they were read in ~180 places
//! across the loop's files (`Instant::now`, `.elapsed()`, `SystemTime::now`,
//! `limit::unix_now`, `journal::unix_ms`, `thread::sleep`), each one a hole a
//! recorder would miss and a replay would answer from the real clock. Now the
//! loop holds one [`Fx`] (`Session::fx`, cloned into the sink that stamps the
//! journal) and reads every clock through it; the `supervise-fx` source guard
//! (`aterm-census`) keeps a direct read out of the supervisor's shipped code,
//! so a new one is a finding, not a silent hole.
//!
//! **The other doors.** Beside the clocks the door carries every other kind
//! of value the loop observes, each by the method a recorder needs it by
//! (the replay plan's §4.3):
//! * a ROUND TRIP ([`Fx::exchange`]): a request out and a reply back — the
//!   control socket's (`tape::ctl::TapedCtl` wraps the loop's transport in
//!   it), a sleep ([`Fx::sleep`]: its length out,
//!   nothing back) and a WRITE whose outcome the loop may read
//!   ([`Fx::bound`]), performed by its live call and never by a replay;
//! * an OUTPUT the loop produced ([`Fx::emit`]): a journal row, a ledger row,
//!   a decision's mark ([`Fx::decide`]);
//! * a FLAG another thread raises ([`Fx::raised`]): the hosted loop's stop
//!   and its hand-over;
//! * a FILE read ([`Fx::read_file`]): the ledger and journal a loop seeds
//!   from, the standing rules;
//! * a NOTE for a reader of the tape ([`Fx::note`]), which a replay skips.
//!
//! **Derived doors: the looks at the machine.** What the loop and its policy
//! read of the machine itself — a folder's names, where a path's links lead,
//! the worker's process, git's configuration as the worker loads it, Claude
//! Code's settings and the shell's startup files, a zone's offset — crosses
//! as a ROUND TRIP
//! too, at `probe.<name>`, but at the level of what was CONCLUDED: the
//! request names what was asked (a path as the run's roots write it, the
//! worker by its digest) and the reply is the answer (a twin's path, a
//! projected git view, a verdict, a number), never the bytes read to reach
//! it. The approval policy's looks go through its probe
//! (`policy::probe::PolicyProbe`, which [`Fx::policy_probe`] hands it); the
//! loop's own through [`Fx::tz_offset`], [`Fx::zone_offset`] and
//! [`Fx::local_offset_at`].
//! A derived door's live body runs below the door: what it reads — the
//! worker's environment among it — reaches no tape. The `supervise-fx` guard
//! (phase 3) names a
//! direct look anywhere else in the supervisor.
//!
//! **Live is the shipped loop, and changes nothing.** [`ClockSource::Os`] is
//! exactly what the call sites read before: `Instant::now()`,
//! `thread::sleep`, and the same `aterm_time` readers behind
//! `limit::unix_now{,_ms}` and `journal::unix_ms`. In [`Mode::Live`] every
//! door calls its live world and returns what it returned, takes no lock and
//! builds nothing for a tape: a request, an output or a note is a closure the
//! live door never calls.
//!
//! **Record** ([`Fx::recording`]) does the same live call and returns the
//! same live value — never one decoded back from the tape, so not even a bug
//! in the recorder can change what the loop sees — and then hands the value
//! to the run's recorder ([`super::tape::core::TapeCore`]), which tapes it
//! in the order the loop observed it. Every clock read is taped (a `Mono` or
//! `Clock` record of a few bytes); no read is skipped and none is invented,
//! so recording changes no clock the loop reads (the plan's rejected "tick
//! rule" would have frozen time across every blocking call that is not a
//! door). A door reached INSIDE another door's live call runs live and is
//! taped nowhere (the nesting rule, the plan's §4.4): the outer door's record
//! already holds its effect, and a replay, which never runs the outer live
//! call, would never ask the inner one.
//!
//! **Replay** ([`Fx::replay`]) reads nothing live: every clock, round trip,
//! file, flag and derived look is answered by the tape's record in the order
//! the recorded loop observed it (`tape::replay`'s core, over astream's replay
//! seam), and every request and output the replayed loop makes is checked
//! against the recorded one where it occurs; a live call handed to the door
//! is never made, and a write is compared, never performed. The files a run
//! writes beside its loop are named and never opened ([`Fx::opens_files`]).
//! At the first divergence the door unwinds the loop from where it stands.
//!
//! **Two wall clocks, as before.** The loop's own wall clock ([`Fx::wall_ms`],
//! [`Fx::wall_s`]: what `Session::now_unix{,_ms}` read) is the one a test fixes
//! ([`ScriptedClock`]); the clock a WRITTEN ROW is stamped with, and a stamped
//! row's age is measured against ([`Fx::stamp_ms`]: the journal's `t`, the
//! ledger's `ts`, a ledger seed's ages), is the system's even under a fixed
//! clock — a test's fixed clock never
//! stamped a journal row, and routing the stamps through it would both change
//! every journaled `t` in the tests and consume the fixed clock's staged jump
//! at a different read (the jump lands at "the next read" of the loop's clock,
//! so an extra reader moves the Mac's sleep). Under [`ClockSource::Os`] the two
//! are the same reading. On a tape each is a `Clock` record of the wall
//! nanoseconds it read; the recorder's own look at the clocks — the stamp it
//! puts on a round trip — is a PEEK ([`ClockSource::peek`]) that never makes
//! a read, so a scripted clock's staged jump lands where it lands untaped.
//!
//! **The machine's count of its sleep** ([`Fx::asleep`], [`Fx::last_sleep`]):
//! how long the machine slept since boot (the clock that counts a sleep less
//! the one a sleep stops) and its record of its last sleep, derived doors the
//! loop reads at its first look, where its watch suspects a sleep, and at an
//! act. The test clocks keep a count of their own, off until a test counts
//! ([`ScriptedClock::count_sleeps`], [`VirtualClock::count_sleeps`]): then a
//! sleep they stage is counted and a step of the fixed clock is time awake.
//!
//! **The test clocks fold in here.** The fixed clock the tests stage
//! (`Session::set_clock`, `set_clock_at_ms`, `set_clock_jump`,
//! `set_clock_running`) was five `cfg(test)` fields on `Session` and a
//! `cfg(test)` branch in each wall reader; it is [`ScriptedClock`] now, with
//! the same arithmetic — including the SWAP ON READ that stages a Mac's sleep
//! between two reads — and the setters forward to it, so no test changed. A
//! [`VirtualClock`] (tests) runs the monotonic and wall clocks off sleeps
//! alone: an hour-long outage is ridden out in no real time, and a sleep that
//! bypasses the door would hang for real.

use std::panic::AssertUnwindSafe;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[cfg(test)]
use std::sync::{Mutex, PoisonError};

use super::policy::box_facts::BoxFacts;
use super::policy::probe::{LiveProbe, PolicyProbe, ReplayProbe, TapedProbe};
use super::policy::sleeps::SleepStamp;
use super::run::IdleHost;
use super::screen::Screen;
use super::tape::codec::{CodecError, End, Stamp};
use super::tape::core::{TapeCore, TapeValue};
use super::tape::doors;
use super::tape::frame::Hint;
use super::tape::host::{HostReply, ReplayHost, TapedHost};
use super::tape::replay::ReplayCore;

/// The loop's door to its clocks and the rest of its world. Cheap to clone
/// (an `Arc` inside at most): the session holds one, the journal's sink and
/// each taped transport a clone.
#[derive(Clone, Debug)]
pub(crate) struct Fx {
    clock: ClockSource,
    mode: Mode,
}

/// Where the clocks come from.
#[derive(Clone, Debug)]
pub(crate) enum ClockSource {
    /// The system's: what every call site read before the door.
    Os,
    /// The tests' fixed wall clock over the real monotonic one.
    #[cfg(test)]
    Scripted(Arc<ScriptedClock>),
    /// The tests' virtual clocks, advanced only by sleeps.
    #[cfg(test)]
    Virtual(Arc<VirtualClock>),
}

/// What the door does besides reading its live world ([module docs](self)).
#[derive(Clone, Debug)]
enum Mode {
    /// Passes every read through and keeps nothing.
    Live,
    /// Passes every read through and hands each value to the run's recorder.
    Record(Arc<TapeCore>),
    /// Reads nothing live: every value is the tape's, every request and
    /// output checked against the recorded one ([`ReplayCore`]).
    Replay(Arc<ReplayCore>),
}

impl Fx {
    /// The system's clocks, live: the shipped loop's door.
    pub(crate) const fn os() -> Self {
        Self {
            clock: ClockSource::Os,
            mode: Mode::Live,
        }
    }

    /// A door over `clock`, live.
    #[cfg(test)]
    pub(crate) const fn over(clock: ClockSource) -> Self {
        Self {
            clock,
            mode: Mode::Live,
        }
    }

    /// This door's clocks, recording into `core` from now on: what a run made
    /// with `--tape` reads its world through ([module docs](self)).
    pub(crate) fn recording(&self, core: Arc<TapeCore>) -> Self {
        Self {
            clock: self.clock.clone(),
            mode: Mode::Record(core),
        }
    }

    /// A door that REPLAYS a tape through `core`: every clock, round trip,
    /// file and flag the loop reads is the recorded one, and every request
    /// and output it makes is checked against the recorded one where it
    /// occurs ([module docs](self)). No live call is ever made.
    pub(crate) fn replay(core: Arc<ReplayCore>) -> Self {
        Self {
            clock: ClockSource::Os,
            mode: Mode::Replay(core),
        }
    }

    /// Whether this door replays a tape.
    pub(crate) fn replaying(&self) -> bool {
        matches!(self.mode, Mode::Replay(_))
    }

    /// Whether this door neither records nor replays: the shipped loop's.
    pub(crate) fn is_live(&self) -> bool {
        matches!(self.mode, Mode::Live)
    }

    /// The recorder this door tapes into, if it records.
    pub(crate) fn tape(&self) -> Option<&TapeCore> {
        match &self.mode {
            Mode::Live | Mode::Replay(_) => None,
            Mode::Record(core) => Some(core),
        }
    }

    /// Whether the files a run writes beside its loop (its journal, its
    /// approval ledger) are opened: yes, but on a replay — whose rows are
    /// outputs it compares with the recorded ones, never writes, and whose
    /// roots name directories that do not exist, so an open would only fail
    /// and say so.
    pub(crate) fn opens_files(&self) -> bool {
        !self.replaying()
    }

    /// The run's journal at `path` for a loop on `sid`
    /// ([`super::journal::Journal::open`]), its one warning to `warn`; on a
    /// replay one that is named and never opened ([`Self::opens_files`]).
    pub(crate) fn open_journal(
        &self,
        path: Option<&Path>,
        sid: Option<&str>,
        warn: &mut dyn std::io::Write,
    ) -> super::journal::Journal {
        if self.opens_files() {
            super::journal::Journal::open(path, sid, warn)
        } else {
            super::journal::Journal::named(path, sid)
        }
    }

    /// The run's approval ledger at `path` ([`super::approvals::Ledger::open`]),
    /// as [`Self::open_journal`].
    pub(crate) fn open_ledger(
        &self,
        path: Option<&Path>,
        sid: Option<&str>,
        warn: &mut dyn std::io::Write,
    ) -> super::approvals::Ledger {
        if self.opens_files() {
            super::approvals::Ledger::open(path, sid, warn)
        } else {
            super::approvals::Ledger::named(path, sid)
        }
    }

    /// The approval policy's probe for a decision this door's loop makes
    /// ([`PolicyProbe`]): the live look in the shipped loop, a look that
    /// tapes each derived answer through this door when the run records.
    pub(crate) fn policy_probe(&self) -> Arc<dyn PolicyProbe> {
        match &self.mode {
            Mode::Live => Arc::new(LiveProbe),
            Mode::Record(core) => Arc::new(TapedProbe::new(self.clone(), core.seed())),
            Mode::Replay(core) => Arc::new(ReplayProbe::replaying(self.clone(), core.seed())),
        }
    }

    /// The local zone's offset from UTC now, in seconds
    /// ([`super::limit::tz_offset_s`]: `date +%z`; `fixed`, a test's stand-in,
    /// answers in its place): on a tape, the derived door `probe.tz_offset`,
    /// the number back.
    pub(crate) fn tz_offset(&self, fixed: Option<i64>) -> i64 {
        self.exchange(doors::PROBE_TZ_OFFSET, Vec::new, || {
            fixed.unwrap_or_else(super::limit::tz_offset_s)
        })
    }

    /// Zone `zone`'s offset from UTC at `unix`, by `live` (the session's
    /// reader: [`super::limit::zone_offset_at`], a table in tests): on a
    /// tape, the derived door `probe.zone_offset`.
    pub(crate) fn zone_offset(
        &self,
        live: fn(&str, i64) -> Option<i64>,
        zone: &str,
        unix: i64,
    ) -> Option<i64> {
        self.exchange(
            doors::PROBE_ZONE_OFFSET,
            || format!("{zone} {unix}").into_bytes(),
            || live(zone, unix),
        )
    }

    /// The local zone's offset at `unix`, by `live` (the session's reader:
    /// the machine's `date`, a table in tests): on a tape, the derived door
    /// `probe.local_offset_at`.
    pub(crate) fn local_offset_at(&self, live: fn(i64) -> Option<i64>, unix: i64) -> Option<i64> {
        self.exchange(
            doors::PROBE_LOCAL_OFFSET,
            || unix.to_string().into_bytes(),
            || live(unix),
        )
    }

    /// Where the clocks stand, read WITHOUT advancing them: the monotonic
    /// clock and the wall clock in Unix milliseconds. A recorder's own look
    /// (a tape's origin, a door's stamp), never the loop's.
    pub(crate) fn peek(&self) -> (Instant, i64) {
        match &self.mode {
            Mode::Replay(core) => core.peek(),
            Mode::Live | Mode::Record(_) => self.clock.peek(),
        }
    }

    /// The stamp a door crossed now carries: where the clocks stand, peeked.
    fn stamp(&self, core: &TapeCore) -> Stamp {
        let (at, wall_ms) = self.clock.peek();
        Stamp {
            mono_ns: core.mono_ns(at),
            wall_ms,
        }
    }

    /// The monotonic clock now: `Instant::now()` in the shipped loop.
    pub(crate) fn mono(&self) -> Instant {
        if let Mode::Replay(core) = &self.mode {
            return core.mono();
        }
        let at = self.clock.mono();
        if let Mode::Record(core) = &self.mode {
            core.mono(at);
        }
        at
    }

    /// How long since `t` on the monotonic clock: `t.elapsed()` in the shipped
    /// loop (zero for a `t` in the future, as `elapsed` saturates).
    pub(crate) fn since(&self, t: Instant) -> Duration {
        self.mono().saturating_duration_since(t)
    }

    /// The loop's wall clock, Unix milliseconds: `limit::unix_now_ms()` in the
    /// shipped loop; a test's fixed clock when it set one.
    pub(crate) fn wall_ms(&self) -> i64 {
        if let Mode::Replay(core) = &self.mode {
            return core.clock().div_euclid(1_000_000);
        }
        let ms = self.clock.wall_ms();
        if let Mode::Record(core) = &self.mode {
            core.clock(ms.wrapping_mul(1_000_000));
        }
        ms
    }

    /// The loop's wall clock, Unix seconds: `limit::unix_now()` in the
    /// shipped loop; a test's fixed clock when it set one.
    pub(crate) fn wall_s(&self) -> i64 {
        if let Mode::Replay(core) = &self.mode {
            return core.clock().div_euclid(1_000_000_000);
        }
        let s = self.clock.wall_s();
        if let Mode::Record(core) = &self.mode {
            core.clock(s.wrapping_mul(1_000_000_000));
        }
        s
    }

    /// The wall clock a written row is stamped with, and a stamped row's age
    /// is measured against, Unix milliseconds: `journal::unix_ms()` — the
    /// system's even under a test's fixed clock (module docs).
    pub(crate) fn stamp_ms(&self) -> i64 {
        if let Mode::Replay(core) = &self.mode {
            return core.clock().div_euclid(1_000_000);
        }
        let ms = self.clock.stamp_ms();
        if let Mode::Record(core) = &self.mode {
            core.clock(ms.wrapping_mul(1_000_000));
        }
        ms
    }

    /// THE MACHINE'S COUNT OF ITS SLEEP NOW ([`SleepStamp`]: how long it
    /// slept since boot, `aterm_uds::process::asleep_since_boot`, and the
    /// boot's name, `boot_session`), `None` where it keeps none — and from
    /// the system's clocks in a test build, whose loops read a sleep only
    /// where a test's clock counts one ([`ScriptedClock::count_sleeps`]). On a
    /// tape, the derived door [`doors::PROBE_ASLEEP`].
    pub(crate) fn asleep(&self) -> Option<SleepStamp> {
        self.exchange(doors::PROBE_ASLEEP, Vec::new, || self.clock.asleep())
    }

    /// THE MACHINE'S LAST SLEEP, as its kernel recorded it: when it slept and
    /// woke, unix ms (`aterm_uds::process::last_sleep_ms`; `None` with none,
    /// where none is kept, and from the system's clocks in a test build). On
    /// a tape, the derived door [`doors::PROBE_LAST_SLEEP`].
    pub(crate) fn last_sleep(&self) -> Option<(i64, i64)> {
        self.exchange(doors::PROBE_LAST_SLEEP, Vec::new, || {
            self.clock.last_sleep()
        })
    }

    /// Sleep `d`: `thread::sleep` in the shipped loop. On a tape, a round
    /// trip at [`doors::SLEEP`] whose request is the length, in nanoseconds.
    pub(crate) fn sleep(&self, d: Duration) {
        self.exchange(
            doors::SLEEP,
            || d.as_nanos().to_string().into_bytes(),
            || self.clock.sleep(d),
        );
    }

    /// A ROUND TRIP through the door named `door`: `live` is the real one (a
    /// control request, a file written), its value returned. On a tape: the
    /// door, the `request` (built only when taping) and the value, stamped
    /// where the clocks stood when it came back — the moment the loop went on
    /// from it. A round trip reached inside another door's live call runs
    /// live and is taped nowhere ([module docs](self)).
    pub(crate) fn exchange<A: TapeValue>(
        &self,
        door: &str,
        request: impl FnOnce() -> Vec<u8>,
        live: impl FnOnce() -> A,
    ) -> A {
        self.exchange_as(
            door,
            request,
            live,
            |a, _| a.to_tape(),
            |b, _| A::from_tape(b),
        )
    }

    /// A ROUND TRIP INTO THE WINDOW'S HOST at `door` (`host.<name>`), as
    /// [`Self::exchange`]: its answer written with the tape's origin to place
    /// an `Instant` it holds by ([`HostReply`]: a reach's `since`, an
    /// account's wait, `wants_at`), and read back onto a replay's own.
    pub(crate) fn host_exchange<A: HostReply>(
        &self,
        door: &str,
        request: impl FnOnce() -> Vec<u8>,
        live: impl FnOnce() -> A,
    ) -> A {
        self.exchange_as(door, request, live, A::put, A::take)
    }

    /// A NOTIFICATION TO THE WINDOW'S HOST at `door` (`host.<name>`): an
    /// output whose bytes are `request`, its arguments ([`Self::emit`]),
    /// said before `live` tells the host — and a replay tells no host.
    ///
    /// A host that panics while it is told is the twin of a round trip whose
    /// live call panics ([`Self::exchange_as`]), and is said the same way:
    /// the output is on the tape already and the run's end will be the
    /// unwind, but nothing would say which door the run died at, so a replay
    /// — which compares the output, returns, and goes on to a door the tape
    /// no longer holds — would call it behaviour changed (DIVERGED) where a
    /// query that panicked is PANICKED. The tape says the door and the
    /// request (`NOTE_UNWOUND_IN`) before the unwind goes on, and a replay
    /// whose tell of that very notification is the tape's last record has
    /// reached the place the recorded run died (`ReplayCore::emit`). A tell
    /// reached inside another door's live call is said by that door's own
    /// unwind, so it runs as it is (the nesting rule: [`TapeCore::enter`]).
    pub(crate) fn host_tell(&self, door: &str, request: Vec<u8>, live: impl FnOnce()) {
        let core = match &self.mode {
            Mode::Live => return live(),
            Mode::Replay(core) => return core.emit(door, &request),
            Mode::Record(core) => core,
        };
        core.emit(door, || request.clone());
        let Some(entered) = core.enter(door) else {
            return live();
        };
        let told = std::panic::catch_unwind(AssertUnwindSafe(live));
        drop(entered);
        if let Err(panic) = told {
            core.unwound_in(door, &request);
            std::panic::resume_unwind(panic)
        }
    }

    /// The window's host as this door's loop must ask it
    /// (`run::host_door`): the host itself in the shipped loop; the taped
    /// host over it when the run records, so every answer it gives the loop
    /// is on the tape; the replay's host — every answer the tape's, `host`
    /// never asked — when the run replays.
    pub(crate) fn host_door(&self, host: &Arc<dyn IdleHost>) -> Arc<dyn IdleHost> {
        match &self.mode {
            Mode::Live => Arc::clone(host),
            Mode::Record(_) => Arc::new(TapedHost::new(Arc::clone(host), self.clone())),
            Mode::Replay(_) => Arc::new(ReplayHost::new(self.clone())),
        }
    }

    /// [`Self::exchange`]'s body, its value written by `put` and read back by
    /// `take`, each handed the tape's origin.
    fn exchange_as<A>(
        &self,
        door: &str,
        request: impl FnOnce() -> Vec<u8>,
        live: impl FnOnce() -> A,
        put: impl FnOnce(&A, Instant) -> Vec<u8>,
        take: impl FnOnce(&[u8], Instant) -> Result<A, CodecError>,
    ) -> A {
        let core = match &self.mode {
            Mode::Live => return live(),
            Mode::Replay(core) => {
                let reply = core.exchange(door, &request());
                return take(&reply, core.origin())
                    .unwrap_or_else(|e| core.undecodable(door, &e.to_string()));
            }
            Mode::Record(core) => core,
        };
        let Some(entered) = core.enter(door) else {
            return live();
        };
        // A live call that panics (a probe's body, the transport) leaves no
        // value to tape, so the round trip would leave no record and its
        // replay would find the tape over at this door — a divergence, not
        // the recorded panic. The tape says instead where the run died,
        // this door and its request (`NOTE_UNWOUND_IN`), and the unwind goes
        // on unchanged: the panic hook has already run, and the run's end
        // (`End::Unwound`) is the caller's to write.
        let value = match std::panic::catch_unwind(AssertUnwindSafe(live)) {
            Ok(value) => value,
            Err(panic) => {
                drop(entered);
                core.unwound_in(door, &request());
                std::panic::resume_unwind(panic)
            }
        };
        drop(entered);
        let reply = put(&value, core.origin());
        core.exchange(door, request(), reply, self.stamp(core));
        value
    }

    /// An OUTPUT the loop produced at `door` (a journal row, a ledger row): on
    /// a tape, its bytes, built only when taping; the output itself is the
    /// caller's to write, and a replay compares instead.
    pub(crate) fn emit(&self, door: &str, bytes: impl FnOnce() -> Vec<u8>) {
        match &self.mode {
            Mode::Live => {}
            Mode::Record(core) => core.emit(door, bytes),
            Mode::Replay(core) => core.emit(door, &bytes()),
        }
    }

    /// A FLAG another thread raises, read through `door`: `live` is the real
    /// read. On a tape only its first true read, the reads that answered
    /// false counted in it (astream's `raised` door, which latches: right for
    /// a flag whose rise is final for the loop — the stop — or one it reads
    /// once — the hand-over at the run's end).
    pub(crate) fn raised(&self, door: &str, live: impl FnOnce() -> bool) -> bool {
        let core = match &self.mode {
            Mode::Live => return live(),
            Mode::Replay(core) => return core.raised(door),
            Mode::Record(core) => core,
        };
        let Some(entered) = core.enter(door) else {
            return live();
        };
        let up = live();
        drop(entered);
        core.raised(door, up, || self.stamp(core))
    }

    /// A NOTE of kind `kind` for a reader of the tape (a turn-end fold's
    /// state, the context a `why` answer prints), built only when taping. A
    /// replay never consumes one.
    pub(crate) fn note(&self, kind: &str, text: impl FnOnce() -> String) {
        if let Mode::Record(core) = &self.mode {
            core.note(kind, text);
        }
    }

    /// A FILE's bytes, `std::fs::read`: on a tape, the bytes or the error's
    /// kind, under the path as the run's roots write it.
    pub(crate) fn read_file(&self, path: &Path) -> std::io::Result<Vec<u8>> {
        let core = match &self.mode {
            Mode::Live => return std::fs::read(path),
            Mode::Replay(core) => return core.file(path),
            Mode::Record(core) => core,
        };
        let Some(entered) = core.enter("file") else {
            return std::fs::read(path);
        };
        let read = std::fs::read(path);
        drop(entered);
        core.file(path, read)
    }

    /// A FILE's text, as `std::fs::read_to_string` reads it ([`Self::read_file`]):
    /// `None` when it cannot be read or is not UTF-8.
    pub(crate) fn read_text(&self, path: &Path) -> Option<String> {
        self.read_file(path)
            .ok()
            .and_then(|bytes| String::from_utf8(bytes).ok())
    }

    /// `path` as the run's roots write it on a tape (`@ledger/s-1.jsonl`);
    /// as it is when nothing tapes.
    pub(crate) fn path_bytes(&self, path: &Path) -> Vec<u8> {
        match &self.mode {
            Mode::Live => path.as_os_str().as_encoded_bytes().to_vec(),
            Mode::Record(core) => core.path(path).into_bytes(),
            Mode::Replay(core) => core.path(path).into_bytes(),
        }
    }

    /// Cut a per-session file back to its bound ([`super::approvals::bound`]):
    /// on a tape a round trip at [`doors::WRITE_BOUND`] naming the file,
    /// which a replay never performs.
    pub(crate) fn bound(&self, path: &Path) {
        self.exchange(
            doors::WRITE_BOUND,
            || self.path_bytes(path),
            || super::approvals::bound(path),
        );
    }

    /// Close the tape's open frame now, for `why` ([`Hint`]): an act is on
    /// disk before what follows it, a wait's tape before the wait.
    pub(crate) fn flush(&self, why: Hint) {
        if let Mode::Record(core) = &self.mode {
            core.flush(why);
        }
    }

    /// The screen the loop just parsed from the last round trip: what a
    /// decision's mark names as the screen it judged ([`Self::decide`]).
    pub(crate) fn screen_read(&self, screen: &Screen) {
        match &self.mode {
            Mode::Live => {}
            Mode::Record(core) => core.screen_read(screen.seq, screen.generation.as_deref()),
            Mode::Replay(core) => core.screen_read(screen.seq, screen.generation.as_deref()),
        }
    }

    /// A DECISION's mark at `door` (`decide.approval`, `decide.question`,
    /// `decide.turn_end`): the screen it judged, the rule that decided and
    /// the outcome, rendered by `outcome` with every `Instant` as an offset
    /// from the tape's origin (the renderer it is handed), and for a box the
    /// FACTS it was decided on (`facts`: captured filesystem paths use the
    /// run's root text and the worker uses the tape's digest, [`BoxFacts::to_json`])
    /// — each built only when taping.
    pub(crate) fn decide(
        &self,
        door: &str,
        screen: &Screen,
        rule: Option<&str>,
        outcome: impl FnOnce(&dyn Fn(Instant) -> String) -> String,
        facts: Option<&BoxFacts>,
    ) {
        match &self.mode {
            Mode::Live => {}
            Mode::Record(core) => {
                let text = outcome(&|at| core.offset_text(at));
                let facts = facts.map(|f| f.to_json(core.seed(), &|p| core.path(p).into_bytes()));
                core.decide(
                    door,
                    screen.seq,
                    screen.generation.as_deref(),
                    rule,
                    &text,
                    facts,
                );
            }
            Mode::Replay(core) => {
                let text = outcome(&|at| core.offset_text(at));
                let facts = facts.map(|f| f.to_json(core.seed(), &|p| core.path(p).into_bytes()));
                core.decide(
                    door,
                    screen.seq,
                    screen.generation.as_deref(),
                    rule,
                    &text,
                    facts,
                );
            }
        }
    }

    /// The run's end, said on its tape ([`End`]), which then makes the tape
    /// durable. Nothing is taped after it.
    pub(crate) fn finish(&self, end: End) {
        if let Mode::Record(core) = &self.mode {
            core.end(end);
        }
    }

    /// The test clock this door reads, made a [`ScriptedClock`] first if it
    /// was the system's (a fresh one reads the system's until a setter fixes
    /// it, so the swap changes nothing on its own). `None` under a
    /// [`VirtualClock`], which a fixed clock would contradict.
    #[cfg(test)]
    pub(crate) fn scripted(&mut self) -> Option<&ScriptedClock> {
        if matches!(self.clock, ClockSource::Os) {
            self.clock = ClockSource::Scripted(Arc::default());
        }
        match &self.clock {
            ClockSource::Scripted(c) => Some(c),
            ClockSource::Os | ClockSource::Virtual(_) => None,
        }
    }

    /// [`Self::scripted`]'s clock as a handle a test keeps past the session's
    /// borrow (a stand-in host that jumps it from inside the run).
    #[cfg(test)]
    pub(crate) fn scripted_handle(&mut self) -> Option<Arc<ScriptedClock>> {
        self.scripted()?;
        match &self.clock {
            ClockSource::Scripted(c) => Some(Arc::clone(c)),
            ClockSource::Os | ClockSource::Virtual(_) => None,
        }
    }
}

impl ClockSource {
    fn mono(&self) -> Instant {
        match self {
            Self::Os => Instant::now(),
            #[cfg(test)]
            Self::Scripted(c) => c.mono(),
            #[cfg(test)]
            Self::Virtual(v) => v.mono(),
        }
    }

    /// The monotonic clock and the loop's wall clock (Unix milliseconds),
    /// read without making a read: a scripted clock's staged jump is not
    /// swapped in, a virtual clock does not move ([module docs](self)).
    fn peek(&self) -> (Instant, i64) {
        match self {
            Self::Os => (Instant::now(), super::limit::unix_now_ms()),
            #[cfg(test)]
            Self::Scripted(c) => (c.mono(), c.peek_wall_ms()),
            #[cfg(test)]
            Self::Virtual(v) => (v.mono(), v.wall_ms()),
        }
    }

    fn wall_ms(&self) -> i64 {
        match self {
            Self::Os => super::limit::unix_now_ms(),
            #[cfg(test)]
            Self::Scripted(c) => c.wall_ms(),
            #[cfg(test)]
            Self::Virtual(v) => v.wall_ms(),
        }
    }

    fn wall_s(&self) -> i64 {
        match self {
            Self::Os => super::limit::unix_now(),
            #[cfg(test)]
            Self::Scripted(c) => c.wall_s(),
            #[cfg(test)]
            Self::Virtual(v) => v.wall_ms().div_euclid(1000),
        }
    }

    fn stamp_ms(&self) -> i64 {
        match self {
            Self::Os => super::journal::unix_ms(),
            #[cfg(test)]
            Self::Scripted(_) => super::journal::unix_ms(),
            #[cfg(test)]
            Self::Virtual(v) => v.wall_ms(),
        }
    }

    fn sleep(&self, d: Duration) {
        match self {
            Self::Os => std::thread::sleep(d),
            #[cfg(test)]
            Self::Scripted(_) => std::thread::sleep(d),
            #[cfg(test)]
            Self::Virtual(v) => v.advance(d),
        }
    }

    /// [`Fx::asleep`]'s live reading.
    fn asleep(&self) -> Option<SleepStamp> {
        match self {
            Self::Os => os_asleep(),
            #[cfg(test)]
            Self::Scripted(c) => c.asleep(),
            #[cfg(test)]
            Self::Virtual(v) => v.asleep(),
        }
    }

    /// [`Fx::last_sleep`]'s live reading.
    fn last_sleep(&self) -> Option<(i64, i64)> {
        match self {
            Self::Os => os_last_sleep(),
            #[cfg(test)]
            Self::Scripted(c) => c.script().last_sleep,
            #[cfg(test)]
            Self::Virtual(v) => v.sleeps().1,
        }
    }
}

/// The machine's count of its sleep, read off the kernel.
#[cfg(not(test))]
fn os_asleep() -> Option<SleepStamp> {
    aterm_uds::process::asleep_since_boot().map(|asleep| SleepStamp {
        asleep,
        boot: aterm_uds::process::boot_session(),
    })
}

/// A test build's system clocks count no sleep: a loop over them reads a
/// sleep by its clocks alone, as on a machine that keeps no count, and no
/// test reads the machine it runs on (the inert arm of a platform probe).
#[cfg(test)]
fn os_asleep() -> Option<SleepStamp> {
    None
}

/// The machine's record of its last sleep, read off the kernel.
#[cfg(not(test))]
fn os_last_sleep() -> Option<(i64, i64)> {
    aterm_uds::process::last_sleep_ms()
}

/// A test build's system clocks keep no record of a sleep ([`os_asleep`]).
#[cfg(test)]
fn os_last_sleep() -> Option<(i64, i64)> {
    None
}

/// THE TESTS' FIXED WALL CLOCK, over the real monotonic clock (and the time a
/// scripted worker skipped, [`Self::skip`]) and real sleeps: the knobs that were `Session`'s `cfg(test)` clock fields, with the same
/// arithmetic. Unset ([`Self::set_at_ms`] never called), it reads the system's
/// wall clock, as a session whose test fixed none did.
#[cfg(test)]
#[derive(Debug, Default)]
pub(crate) struct ScriptedClock {
    script: Mutex<Script>,
}

#[cfg(test)]
#[derive(Debug, Default)]
struct Script {
    /// The fixed clock, Unix seconds; `None` reads the system's.
    fixed: Option<i64>,
    /// Its milliseconds past the whole second.
    phase_ms: i64,
    /// How far it jumps ahead after its next read ([`ScriptedClock::set_jump`]).
    jump: i64,
    /// The jump once that read is made: swapped in at every read, so the read
    /// after `set_jump` still sees the old time and every later one the jump.
    skew: i64,
    /// The instant it starts RUNNING from, keeping pace with the monotonic
    /// clock ([`ScriptedClock::set_running`]); `None`: it stands still.
    runs_from: Option<Instant>,
    /// Where the monotonic clock STANDS ([`ScriptedClock::stand_mono`]);
    /// `None`: the system's.
    mono_stands: Option<Instant>,
    /// The time SKIPPED ([`ScriptedClock::skip`]), which the monotonic clock
    /// stands ahead of the system's (or of where it stands) by.
    skipped: Duration,
    /// The jump the last read saw (its `skew`): a sleep staged lands in the
    /// machine's count at the read that first shows it (`staged_ms`).
    seen: i64,
    /// THE MACHINE'S COUNT OF ITS SLEEP, in milliseconds
    /// ([`ScriptedClock::asleep`]); `None` — a machine that keeps no count,
    /// whose loop reads a sleep by its clocks alone, as every test that
    /// counts none did — until a test counts ([`ScriptedClock::count_sleeps`]).
    counted_ms: Option<i64>,
    /// A sleep staged by a jump ([`ScriptedClock::set_jump`],
    /// [`ScriptedClock::slept`]) and not yet shown: counted at the read of the
    /// wall clock that shows the jump, as the machine counts a sleep as the
    /// wall clock runs through it.
    staged_ms: i64,
    /// The boot the count is on ([`ScriptedClock::set_boot`]); `None`:
    /// [`TEST_BOOT`].
    boot: Option<String>,
    /// The machine's record of its last sleep (unix ms, asleep and woke):
    /// the last one a counting test staged, or what it set
    /// ([`ScriptedClock::set_last_sleep`]).
    last_sleep: Option<(i64, i64)>,
}

/// The boot a counting test clock counts on unless the test names another.
#[cfg(test)]
pub(crate) const TEST_BOOT: &str = "test-boot";

#[cfg(test)]
impl Script {
    /// The monotonic clock now: where it stands, else the system's, the time
    /// skipped ahead.
    fn mono(&self) -> Instant {
        self.mono_stands.unwrap_or_else(Instant::now) + self.skipped
    }

    /// A sleep of `secs` staged from `at` (the fixed clock's milliseconds
    /// then): counted once a read shows it, and the machine's record of its
    /// last sleep, while it counts.
    fn staged_last(&mut self, at: Option<i64>, secs: i64) {
        if self.counted_ms.is_some()
            && let Some(at) = at
        {
            self.staged_ms = self.staged_ms.saturating_add(secs.saturating_mul(1000));
            self.last_sleep = Some((at, at.saturating_add(secs.saturating_mul(1000))));
        }
    }
}

#[cfg(test)]
impl ScriptedClock {
    fn script(&self) -> std::sync::MutexGuard<'_, Script> {
        self.script.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Fix the clock at `now_ms`, Unix milliseconds.
    pub(crate) fn set_at_ms(&self, now_ms: i64) {
        let mut s = self.script();
        s.fixed = Some(now_ms.div_euclid(1000));
        s.phase_ms = now_ms.rem_euclid(1000);
    }

    /// The fixed clock jumps `jump` seconds ahead after its next read — the
    /// wall clock of a Mac that sleeps between two steps of the loop's wait,
    /// while the monotonic clock stands still. Once: the Mac wakes.
    pub(crate) fn set_jump(&self, jump: i64) {
        let at = self.read_ms(true);
        let mut s = self.script();
        s.jump = jump;
        s.skew = 0;
        s.staged_ms = 0;
        s.staged_last(at, jump);
    }

    /// The Mac slept `secs` NOW: the fixed clock's next read and every one
    /// after it is that much later, the monotonic clock standing still — a
    /// sleep staged from inside a run (a test's stand-in host), where
    /// [`Self::set_jump`] stages one before the read that is to see it.
    pub(crate) fn slept(&self, secs: i64) {
        let at = self.read_ms(true);
        let mut s = self.script();
        s.jump += secs;
        s.skew += secs;
        s.staged_last(at, secs);
    }

    /// THE MACHINE COUNTS ITS SLEEP from now ([`Self::asleep`]), the count
    /// standing at `total`: every sleep the clock stages from here on is
    /// counted ([`Self::slept`], [`Self::set_jump`] once its jump stands,
    /// [`Self::count_slept`]), and nothing else — a step of the fixed clock
    /// ([`Self::set_at_ms`]) is time awake.
    pub(crate) fn count_sleeps(&self, total: Duration) {
        let mut s = self.script();
        let total = i64::try_from(total.as_millis()).unwrap_or(i64::MAX);
        s.counted_ms = Some(total);
        s.staged_ms = 0;
    }

    /// The fixed clock is STEPPED `jump` seconds ahead after its next read,
    /// as [`Self::set_jump`] stages it, but no sleep: the machine's count
    /// stands ([`Self::count_sleeps`]) — a person or a time server set the
    /// clock on.
    pub(crate) fn set_step(&self, jump: i64) {
        let mut s = self.script();
        s.jump = jump;
        s.skew = 0;
        s.staged_ms = 0;
    }

    /// The Mac slept `secs` up to now, the fixed clock already moved past it
    /// by the test ([`Self::set_at_ms`]): counted, and the machine's record of
    /// its last sleep (the `secs` before now). Only while counting.
    pub(crate) fn count_slept(&self, secs: i64) {
        let at = self.read_ms(true);
        let mut s = self.script();
        if let Some(c) = &mut s.counted_ms {
            *c = c.saturating_add(secs.saturating_mul(1000));
            s.last_sleep = at.map(|now| (now - secs * 1000, now));
        }
    }

    /// The boot the count is on ([`TEST_BOOT`] unless set): another boot's
    /// count compares with none of this one's.
    pub(crate) fn set_boot(&self, boot: &str) {
        self.script().boot = Some(boot.to_string());
    }

    /// The machine's record of its last sleep (unix ms, asleep and woke), or
    /// none (a machine that keeps none: Linux).
    pub(crate) fn set_last_sleep(&self, last: Option<(i64, i64)>) {
        self.script().last_sleep = last;
    }

    /// The machine's count of its sleep now, a staged sleep in it once a
    /// read of the wall clock showed it; `None` until a test counts
    /// ([`Self::count_sleeps`]).
    fn asleep(&self) -> Option<SleepStamp> {
        let s = self.script();
        let ms = s.counted_ms?;
        Some(SleepStamp {
            asleep: Duration::from_millis(u64::try_from(ms).unwrap_or(0)),
            boot: Some(s.boot.clone().unwrap_or_else(|| TEST_BOOT.to_string())),
        })
    }

    /// The fixed clock RUNS from now, as the system's does.
    pub(crate) fn set_running(&self) {
        let mut s = self.script();
        s.runs_from = Some(s.mono());
    }

    /// SKIP `d`: the monotonic clock jumps `d` ahead, for good, and keeps
    /// the system's pace from there (a fixed clock that runs, too). What a
    /// scripted worker whose screen sits answers a wait with (`Mock::waits`):
    /// the whole time asked passed at once, as a server holding a still
    /// screen would have it, instead of a nap of it.
    pub(crate) fn skip(&self, d: Duration) {
        let mut s = self.script();
        s.skipped = s.skipped.saturating_add(d);
    }

    /// The monotonic clock now ([`Self::stand_mono`], [`Self::skip`]).
    pub(crate) fn mono(&self) -> Instant {
        self.script().mono()
    }

    /// The monotonic clock STANDS from now, as the fixed wall clock does: a
    /// test that moves the policy's instants itself at each step of its
    /// clock ([`super::policy::turn_end::TurnEndState::slept`]) reads no real
    /// time between two steps, however long a loaded machine takes over them.
    pub(crate) fn stand_mono(&self) {
        self.script().mono_stands = Some(Instant::now());
    }

    /// The fixed clock's milliseconds now, the staged jump's swap made (or
    /// not, for a `peek`).
    fn read_ms(&self, peek: bool) -> Option<i64> {
        let mut s = self.script();
        let fixed = s.fixed?;
        let run = s.runs_from.map_or(0, |from| {
            i64::try_from(s.mono().saturating_duration_since(from).as_millis()).unwrap_or(0)
        });
        let skew = if peek {
            s.skew
        } else {
            let jump = s.jump;
            let skew = std::mem::replace(&mut s.skew, jump);
            if skew != s.seen {
                let staged = std::mem::take(&mut s.staged_ms);
                if let Some(c) = &mut s.counted_ms {
                    *c = c.saturating_add(staged);
                }
            }
            s.seen = skew;
            skew
        };
        Some((fixed + skew) * 1000 + s.phase_ms + run)
    }

    /// The loop's wall clock, Unix milliseconds: the fixed clock (its whole
    /// seconds, the staged jump swapped in, plus its phase and its run), else
    /// the system's.
    fn wall_ms(&self) -> i64 {
        self.read_ms(false)
            .unwrap_or_else(super::limit::unix_now_ms)
    }

    /// The loop's wall clock, Unix seconds: the fixed clock's milliseconds
    /// (one read: the swap is made) floored, else the system's seconds.
    fn wall_s(&self) -> i64 {
        self.read_ms(false)
            .map_or_else(super::limit::unix_now, |ms| ms.div_euclid(1000))
    }

    /// [`Self::wall_ms`] WITHOUT the read: the staged jump is not swapped in,
    /// so a reader that must not change the run (a tape's door stamps) can
    /// look at the clock as often as it likes.
    pub(crate) fn peek_wall_ms(&self) -> i64 {
        self.read_ms(true).unwrap_or_else(super::limit::unix_now_ms)
    }
}

/// THE TESTS' VIRTUAL CLOCKS: the monotonic and the wall clock both run off
/// [`Self::advance`] alone, which every sleep through the door calls. No real
/// time passes, so a loop that waits out an hour does so at once — and a sleep
/// that bypasses the door sleeps for real, which a test's hang detector names.
#[cfg(test)]
#[derive(Debug)]
pub(crate) struct VirtualClock {
    origin: Instant,
    wall_origin_ms: i64,
    offset: Mutex<Duration>,
    /// The machine's sleeps ([`Self::sleep_mac`]).
    asleep: Mutex<VirtualSleeps>,
}

/// A [`VirtualClock`]'s sleeps of the machine.
#[cfg(test)]
#[derive(Debug, Default)]
struct VirtualSleeps {
    /// The wall clock's lead on the monotonic one: every sleep.
    slept: Duration,
    /// Whether the machine counts them ([`VirtualClock::count_sleeps`]).
    counts: bool,
    /// The last one (unix ms, asleep and woke).
    last: Option<(i64, i64)>,
}

#[cfg(test)]
impl VirtualClock {
    /// Virtual clocks standing at `origin` (monotonic) and `wall_ms` (Unix
    /// milliseconds) until the first sleep.
    pub(crate) fn new(origin: Instant, wall_ms: i64) -> Self {
        Self {
            origin,
            wall_origin_ms: wall_ms,
            offset: Mutex::new(Duration::ZERO),
            asleep: Mutex::default(),
        }
    }

    /// The machine counts its sleep from now ([`Self::asleep`]): a machine
    /// that counts none (the default) has its loop read a sleep by its
    /// clocks alone.
    pub(crate) fn count_sleeps(&self) {
        self.sleeps_mut().counts = true;
    }

    /// THE MACHINE SLEEPS `d` NOW: the wall clock moves on `d`, the monotonic
    /// one does not, and the machine's record of its last sleep is it.
    pub(crate) fn sleep_mac(&self, d: Duration) {
        let from = self.wall_ms();
        let mut s = self.sleeps_mut();
        s.slept = s.slept.saturating_add(d);
        let ms = i64::try_from(d.as_millis()).unwrap_or(i64::MAX);
        s.last = Some((from, from.saturating_add(ms)));
    }

    fn sleeps_mut(&self) -> std::sync::MutexGuard<'_, VirtualSleeps> {
        self.asleep.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The wall clock's lead and the last sleep ([`Self::sleep_mac`]).
    fn sleeps(&self) -> (Duration, Option<(i64, i64)>) {
        let s = self.sleeps_mut();
        (s.slept, s.last.filter(|_| s.counts))
    }

    /// The machine's count of its sleep: every sleep, on [`TEST_BOOT`];
    /// `None` until it counts ([`Self::count_sleeps`]).
    fn asleep(&self) -> Option<SleepStamp> {
        let s = self.sleeps_mut();
        s.counts.then(|| SleepStamp {
            asleep: s.slept,
            boot: Some(TEST_BOOT.to_string()),
        })
    }

    fn offset(&self) -> Duration {
        *self.offset.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// How much virtual time has passed.
    pub(crate) fn passed(&self) -> Duration {
        self.offset()
    }

    fn advance(&self, d: Duration) {
        let mut offset = self.offset.lock().unwrap_or_else(PoisonError::into_inner);
        *offset = offset.saturating_add(d);
    }

    fn mono(&self) -> Instant {
        self.origin + self.offset()
    }

    fn wall_ms(&self) -> i64 {
        let ahead = self.sleeps_mut().slept;
        let ms = i64::try_from(self.offset().saturating_add(ahead).as_millis()).unwrap_or(i64::MAX);
        self.wall_origin_ms.saturating_add(ms)
    }
}

#[cfg(test)]
#[path = "fx_tests.rs"]
mod tests;
