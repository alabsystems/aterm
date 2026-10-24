// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `Fx`: the one door through which the supervising loop reads its clocks.

use std::panic::AssertUnwindSafe;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[cfg(test)]
use std::sync::{Mutex, PoisonError};

use super::policy::box_facts::BoxFacts;
use super::policy::probe::{LiveProbe, PolicyProbe, ReplayProbe, TapedProbe};
use super::policy::sleeps::{Boot, SleepStamp};
use super::run::IdleHost;
use super::screen::Screen;
use super::tape::codec::{CodecError, End, Stamp};
use super::tape::core::{TapeCore, TapeValue};
use super::tape::doors;
use super::tape::frame::Hint;
use super::tape::host::{HostReply, ReplayHost, TapedHost};
use super::tape::replay::ReplayCore;

/// The loop's door to its clocks and the rest of its world.
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

/// What the door does besides reading its live world.
#[derive(Clone, Debug)]
enum Mode {
    /// Passes every read through and keeps nothing.
    Live,
    /// Passes every read through and hands each value to the run's recorder.
    Record(Arc<TapeCore>),
    /// Reads nothing live; replays recorded values and checks every request and output.
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

    /// This door's clocks, recording into `core` from now on.
    pub(crate) fn recording(&self, core: Arc<TapeCore>) -> Self {
        Self {
            clock: self.clock.clone(),
            mode: Mode::Record(core),
        }
    }

    /// A door that replays a tape through `core`.
    pub(crate) const fn replay(core: Arc<ReplayCore>) -> Self {
        Self {
            clock: ClockSource::Os,
            mode: Mode::Replay(core),
        }
    }

    /// Whether this door replays a tape.
    pub(crate) const fn replaying(&self) -> bool {
        matches!(self.mode, Mode::Replay(_))
    }

    /// Whether this door neither records nor replays: the shipped loop's.
    pub(crate) const fn is_live(&self) -> bool {
        matches!(self.mode, Mode::Live)
    }

    /// The recorder this door tapes into, if it records.
    pub(crate) fn tape(&self) -> Option<&TapeCore> {
        match &self.mode {
            Mode::Live | Mode::Replay(_) => None,
            Mode::Record(core) => Some(core),
        }
    }

    /// Whether sidecar files (journal and approval ledger) are opened on disk.
    pub(crate) const fn opens_files(&self) -> bool {
        !self.replaying()
    }

    /// The run's journal at `path` for a loop on `sid` ([`super::journal::Journal::open`]).
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

    /// The run's approval ledger at `path`, as [`Self::open_journal`].
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

    /// The approval policy's probe for a decision this door's loop makes ([`PolicyProbe`]).
    pub(crate) fn policy_probe(&self) -> Arc<dyn PolicyProbe> {
        match &self.mode {
            Mode::Live => Arc::new(LiveProbe),
            Mode::Record(core) => Arc::new(TapedProbe::new(self.clone(), core.seed())),
            Mode::Replay(core) => Arc::new(ReplayProbe::replaying(self.clone(), core.seed())),
        }
    }

    /// The local zone's offset from UTC now, in seconds.
    pub(crate) fn tz_offset(&self, fixed: Option<i64>) -> i64 {
        self.exchange(doors::PROBE_TZ_OFFSET, Vec::new, || {
            fixed.unwrap_or_else(super::limit::tz_offset_s)
        })
    }

    /// Zone `zone`'s offset from UTC at `unix`, by `live`.
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

    /// The local zone's offset at `unix`, by `live`.
    pub(crate) fn local_offset_at(&self, live: fn(i64) -> Option<i64>, unix: i64) -> Option<i64> {
        self.exchange(
            doors::PROBE_LOCAL_OFFSET,
            || unix.to_string().into_bytes(),
            || live(unix),
        )
    }

    /// Where the clocks stand, read without advancing them.
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

    /// How long since `t` on the monotonic clock: `t.elapsed` in the shipped loop.
    pub(crate) fn since(&self, t: Instant) -> Duration {
        self.mono().saturating_duration_since(t)
    }

    /// The loop's wall clock, Unix milliseconds.
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

    /// The loop's wall clock, Unix seconds.
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

    /// The wall clock a written row is stamped with.
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

    /// The machine's sleep stamp now ([`SleepStamp`]), or `None` when unavailable.
    pub(crate) fn asleep(&self) -> Option<SleepStamp> {
        self.exchange(doors::PROBE_ASLEEP, Vec::new, || self.clock.asleep())
    }

    /// The same count for an act's row, retaining a replay's exact validated boot text.
    pub(crate) fn asleep_for_ledger(&self) -> (Option<SleepStamp>, Option<String>) {
        self.exchange_as(
            doors::PROBE_ASLEEP,
            Vec::new,
            || (self.clock.asleep(), None),
            |(count, _), _| count.to_tape(),
            |bytes, _| {
                let count = Option::<SleepStamp>::from_tape(bytes)?;
                let spelling = std::str::from_utf8(bytes)
                    .ok()
                    .and_then(|text| text.split_once(' ').map(|(_, boot)| boot))
                    .filter(|text| {
                        count
                            .as_ref()
                            .and_then(|c| c.boot)
                            .is_some_and(|boot| Boot::of(text) == Some(boot))
                    })
                    .map(str::to_owned);
                Ok((count, spelling))
            },
        )
    }

    /// The machine's last sleep, as its kernel recorded it.
    pub(crate) fn last_sleep(&self) -> Option<(i64, i64)> {
        self.exchange(doors::PROBE_LAST_SLEEP, Vec::new, || {
            self.clock.last_sleep()
        })
    }

    /// Sleep `d`: `thread::sleep` in the shipped loop.
    pub(crate) fn sleep(&self, d: Duration) {
        self.exchange(
            doors::SLEEP,
            || d.as_nanos().to_string().into_bytes(),
            || self.clock.sleep(d),
        );
    }

    /// A round trip through the door named `door`.
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

    /// A round trip into the WINDOW'S host at `door` (`host.<name>`), as [`Self::exchange`].
    pub(crate) fn host_exchange<A: HostReply>(
        &self,
        door: &str,
        request: impl FnOnce() -> Vec<u8>,
        live: impl FnOnce() -> A,
    ) -> A {
        if self.replay_lacks_host_door(door)
            && let Some(absent) = A::absent()
        {
            return absent;
        }
        self.exchange_as(door, request, live, A::put, A::take)
    }

    /// Whether this replay's recording build lacked a host door, as its header says.
    pub(crate) fn replay_lacks_host_door(&self, door: &str) -> bool {
        match &self.mode {
            Mode::Replay(core) => core.lacks_door(door),
            Mode::Live | Mode::Record(_) => false,
        }
    }

    /// Whether the build that recorded this run had slice 2's host doors (`host.wakes` in a
    /// replay's header); a live or recording loop always has. Slice 2's additions apply only
    /// where this holds, so an older tape replays as recorded.
    pub(crate) fn slice2_recorded(&self) -> bool {
        !self.replay_lacks_host_door("host.wakes")
    }

    /// A notification to the WINDOW'S host at `door` (`host.<name>`).
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

    /// The window's host as this door's loop must ask it (`run::host_door`).
    pub(crate) fn host_door(&self, host: &Arc<dyn IdleHost>) -> Arc<dyn IdleHost> {
        match &self.mode {
            Mode::Live => Arc::clone(host),
            Mode::Record(_) => Arc::new(TapedHost::new(Arc::clone(host), self.clone())),
            Mode::Replay(_) => Arc::new(ReplayHost::new(self.clone())),
        }
    }

    /// Core of [`Self::exchange`], serializing with `put` and deserializing with `take`.
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

    /// An output the loop produced at `door` (a journal row, a ledger row).
    pub(crate) fn emit(&self, door: &str, bytes: impl FnOnce() -> Vec<u8>) {
        match &self.mode {
            Mode::Live => {}
            Mode::Record(core) => core.emit(door, bytes),
            Mode::Replay(core) => core.emit(door, &bytes()),
        }
    }

    /// A flag another thread raises, read through `door`: `live` is the real read.
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

    /// A note of kind `kind` for a reader of the tape, built only when taping.
    pub(crate) fn note(&self, kind: &str, text: impl FnOnce() -> String) {
        if let Mode::Record(core) = &self.mode {
            core.note(kind, text);
        }
    }

    /// A file's bytes, `std::fs::read`.
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

    /// A file's text, as `std::fs::read_to_string` reads it ([`Self::read_file`]).
    pub(crate) fn read_text(&self, path: &Path) -> Option<String> {
        self.read_file(path)
            .ok()
            .and_then(|bytes| String::from_utf8(bytes).ok())
    }

    /// `path` as the run's roots write it on a tape; as it is when nothing tapes.
    pub(crate) fn path_bytes(&self, path: &Path) -> Vec<u8> {
        match &self.mode {
            Mode::Live => path.as_os_str().as_encoded_bytes().to_vec(),
            Mode::Record(core) => core.path(path).into_bytes(),
            Mode::Replay(core) => core.path(path).into_bytes(),
        }
    }

    /// Cut a per-session file back to its bound ([`super::approvals::bound`]).
    pub(crate) fn bound(&self, path: &Path) {
        self.exchange(
            doors::WRITE_BOUND,
            || self.path_bytes(path),
            || super::approvals::bound(path),
        );
    }

    /// Close the tape's open frame now, for `why` ([`Hint`]).
    pub(crate) fn flush(&self, why: Hint) {
        if let Mode::Record(core) = &self.mode {
            core.flush(why);
        }
    }

    /// The screen the loop just parsed from the last round trip.
    pub(crate) fn screen_read(&self, screen: &Screen) {
        match &self.mode {
            Mode::Live => {}
            Mode::Record(core) => core.screen_read(screen.seq, screen.generation.as_deref()),
            Mode::Replay(core) => core.screen_read(screen.seq, screen.generation.as_deref()),
        }
    }

    /// A DECISION's mark at `door` (`decide.approval`, `decide.question`, `decide.turn_end`).
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

    /// Whether a decision here keeps its RULE TRACE.
    pub(crate) fn tracing(&self) -> bool {
        match &self.mode {
            Mode::Live | Mode::Record(_) => false,
            Mode::Replay(core) => core.tracing(),
        }
    }

    /// The trace of the decision about to be marked.
    pub(crate) fn traced(&self, steps: Vec<super::policy::trace::TraceStep>) {
        if let Mode::Replay(core) = &self.mode {
            core.traced(steps);
        }
    }

    /// The CANDIDATE policy a shadow replay decides each turn end again under.
    pub(crate) fn shadow_policy(&self) -> Option<super::SupervisorConfig> {
        match &self.mode {
            Mode::Live | Mode::Record(_) => None,
            Mode::Replay(core) => core.shadow_policy(),
        }
    }

    /// The candidate's decision on the turn end about to be marked.
    pub(crate) fn shadowed(
        &self,
        rule: Option<&str>,
        outcome: impl FnOnce(&dyn Fn(Instant) -> String) -> String,
    ) {
        if let Mode::Replay(core) = &self.mode {
            let text = outcome(&|at| core.offset_text(at));
            core.shadowed(rule, text);
        }
    }

    /// The run's end, said on its tape ([`End`]), which then makes the tape durable.
    pub(crate) fn finish(&self, end: End) {
        if let Mode::Record(core) = &self.mode {
            core.end(end);
        }
    }

    /// The test clock this door reads, made a [`ScriptedClock`] first if it was the system's.
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

    /// [`Self::scripted`]'s clock as a handle a test keeps past the session's borrow.
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

    /// Monotonic clock and wall clock (Unix ms), peeked without advancing.
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
            Self::Scripted(c) => c.stamp_ms(),
            #[cfg(test)]
            Self::Virtual(v) => v.wall_ms(),
        }
    }

    fn sleep(&self, d: Duration) {
        match self {
            Self::Os => std::thread::sleep(d),
            #[cfg(test)]
            Self::Scripted(c) => c.sleep(d),
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

/// Reads the machine's sleep count and boot session from the kernel.
#[cfg(not(test))]
fn os_asleep() -> Option<SleepStamp> {
    aterm_uds::process::asleep_since_boot().map(|asleep| SleepStamp {
        asleep,
        boot: astream_sys::boot_session().map(Boot::kernel),
    })
}

/// A test build's system clocks count no sleep.
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

#[cfg(test)]
thread_local! {
    /// A test's planted variant of the loop.
    static PLANTED_CLOCK_READ: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Plant (or pull) the extra clock read in the loops this thread runs.
#[cfg(test)]
pub(crate) fn plant_clock_read(on: bool) {
    PLANTED_CLOCK_READ.with(|p| p.set(on));
}

/// Whether this thread's loops read the clock once more before each screen.
#[cfg(test)]
pub(crate) fn planted_clock_read() -> bool {
    PLANTED_CLOCK_READ.with(std::cell::Cell::get)
}

/// The tests' fixed wall clock.
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
    /// The jump once that read is made.
    skew: i64,
    /// The instant it starts running from, keeping pace with the monotonic clock.
    runs_from: Option<Instant>,
    /// Where the monotonic clock stands ([`ScriptedClock::stand_mono`]).
    mono_stands: Option<Instant>,
    /// A sleep through the door skips its length instead of sleeping it.
    sleeps_skip: bool,
    /// The time skipped ([`ScriptedClock::skip`]).
    skipped: Duration,
    /// The jump the last read saw (its `skew`).
    seen: i64,
    /// The machine's count of its sleep, in milliseconds ([`ScriptedClock::asleep`]).
    counted_ms: Option<i64>,
    /// Sleep staged by a jump and not yet observed by a wall-clock read.
    staged_ms: i64,
    /// The boot the count is on ([`ScriptedClock::set_boot`]); `None`: [`TEST_BOOT`].
    boot: Option<[u8; 16]>,
    /// The machine's record of its last sleep (unix ms, asleep and woke).
    last_sleep: Option<(i64, i64)>,
    /// Rows are stamped by the fixed clock, not the system's ([`ScriptedClock::stamp_by_script`]).
    stamps_fixed: bool,
}

/// The boot a counting test clock counts on unless the test names another.
#[cfg(test)]
pub(crate) const TEST_BOOT: [u8; 16] = [
    0x7E, 0x57, 0xB0, 0x07, 0x00, 0x00, 0x40, 0x00, 0x80, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01,
];

/// The boot a test names when the machine restarted between two loops.
#[cfg(test)]
pub(crate) const OTHER_TEST_BOOT: [u8; 16] = [
    0x7E, 0x57, 0xB0, 0x07, 0x00, 0x00, 0x40, 0x00, 0x80, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02,
];

#[cfg(test)]
impl Script {
    /// The monotonic clock now: where it stands, else the system's, the time skipped ahead.
    fn mono(&self) -> Instant {
        self.mono_stands.unwrap_or_else(Instant::now) + self.skipped
    }

    /// A sleep of `secs` staged from `at` (the fixed clock's milliseconds then).
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

    /// The fixed clock jumps `jump` seconds ahead after its next read.
    pub(crate) fn set_jump(&self, jump: i64) {
        let at = self.read_ms(true);
        let mut s = self.script();
        s.jump = jump;
        s.skew = 0;
        s.staged_ms = 0;
        s.staged_last(at, jump);
    }

    /// The Mac slept `secs` now.
    pub(crate) fn slept(&self, secs: i64) {
        let at = self.read_ms(true);
        let mut s = self.script();
        s.jump += secs;
        s.skew += secs;
        s.staged_last(at, secs);
    }

    /// The machine counts its sleep from now ([`Self::asleep`]), the count standing at `total`.
    pub(crate) fn count_sleeps(&self, total: Duration) {
        let mut s = self.script();
        let total = i64::try_from(total.as_millis()).unwrap_or(i64::MAX);
        s.counted_ms = Some(total);
        s.staged_ms = 0;
    }

    /// The fixed clock is stepped `jump` seconds ahead after its next read.
    pub(crate) fn set_step(&self, jump: i64) {
        let mut s = self.script();
        s.jump = jump;
        s.skew = 0;
        s.staged_ms = 0;
    }

    /// The Mac slept `secs` up to now.
    pub(crate) fn count_slept(&self, secs: i64) {
        let at = self.read_ms(true);
        let mut s = self.script();
        if let Some(c) = &mut s.counted_ms {
            *c = c.saturating_add(secs.saturating_mul(1000));
            s.last_sleep = at.map(|now| (now - secs * 1000, now));
        }
    }

    /// The boot the count is on: another boot's count compares with none of this one's.
    pub(crate) fn set_boot(&self, boot: [u8; 16]) {
        self.script().boot = Some(boot);
    }

    /// The machine's record of its last sleep, or none.
    pub(crate) fn set_last_sleep(&self, last: Option<(i64, i64)>) {
        self.script().last_sleep = last;
    }

    /// The machine's sleep count now, including any staged sleep already observed.
    fn asleep(&self) -> Option<SleepStamp> {
        let s = self.script();
        let ms = s.counted_ms?;
        Some(SleepStamp {
            asleep: Duration::from_millis(u64::try_from(ms).unwrap_or(0)),
            boot: Some(Boot::kernel(s.boot.unwrap_or(TEST_BOOT))),
        })
    }

    /// The fixed clock runs from now, as the system's does.
    pub(crate) fn set_running(&self) {
        let mut s = self.script();
        s.runs_from = Some(s.mono());
    }

    /// Skip `d`: the monotonic clock jumps `d` ahead.
    pub(crate) fn skip(&self, d: Duration) {
        let mut s = self.script();
        s.skipped = s.skipped.saturating_add(d);
    }

    /// The monotonic clock now ([`Self::stand_mono`], [`Self::skip`]).
    pub(crate) fn mono(&self) -> Instant {
        self.script().mono()
    }

    /// The monotonic clock stands from now, as the fixed wall clock does.
    pub(crate) fn stand_mono(&self) {
        self.script().mono_stands = Some(Instant::now());
    }

    /// The clocks RUN on the RUN'S steps alone from now.
    pub(crate) fn run_virtual(&self) {
        let mut s = self.script();
        s.mono_stands = Some(s.mono_stands.unwrap_or_else(Instant::now));
        s.sleeps_skip = true;
    }

    /// Whether the clocks run on the run's steps alone ([`Self::run_virtual`]).
    pub(crate) fn is_virtual(&self) -> bool {
        self.script().sleeps_skip
    }

    /// Sleep `d`: skipped on a clock that runs on the run's steps, else slept.
    pub(crate) fn sleep(&self, d: Duration) {
        if self.script().sleeps_skip {
            self.skip(d);
        } else {
            std::thread::sleep(d);
        }
    }

    /// The fixed clock's milliseconds now, the staged jump's swap made (or not, for a `peek`).
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

    /// The loop's wall clock, Unix milliseconds.
    fn wall_ms(&self) -> i64 {
        self.read_ms(false)
            .unwrap_or_else(super::limit::unix_now_ms)
    }

    /// The loop's wall clock, Unix seconds.
    fn wall_s(&self) -> i64 {
        self.read_ms(false)
            .map_or_else(super::limit::unix_now, |ms| ms.div_euclid(1000))
    }

    /// [`Self::wall_ms`] without the read.
    pub(crate) fn peek_wall_ms(&self) -> i64 {
        self.read_ms(true).unwrap_or_else(super::limit::unix_now_ms)
    }

    /// Rows written from now are stamped by the fixed clock (a replay whose loops a ledger spans).
    pub(crate) fn stamp_by_script(&self) {
        self.script().stamps_fixed = true;
    }

    /// The stamp a written row carries: the system's clock, or the fixed one's ([`Self::stamp_by_script`]).
    fn stamp_ms(&self) -> i64 {
        if self.script().stamps_fixed {
            self.peek_wall_ms()
        } else {
            super::journal::unix_ms()
        }
    }
}

/// The tests' virtual clocks.
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
    /// Virtual clocks standing at `origin` and `wall_ms` until the first sleep.
    pub(crate) fn new(origin: Instant, wall_ms: i64) -> Self {
        Self {
            origin,
            wall_origin_ms: wall_ms,
            offset: Mutex::new(Duration::ZERO),
            asleep: Mutex::default(),
        }
    }

    /// The machine counts its sleep from now ([`Self::asleep`]).
    pub(crate) fn count_sleeps(&self) {
        self.sleeps_mut().counts = true;
    }

    /// The machine sleeps `d` now.
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

    /// The machine's count of its sleep.
    fn asleep(&self) -> Option<SleepStamp> {
        let s = self.sleeps_mut();
        s.counts.then(|| SleepStamp {
            asleep: s.slept,
            boot: Some(Boot::kernel(TEST_BOOT)),
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
