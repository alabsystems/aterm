// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE RECORDER: what a run's door ([`crate::supervise::fx::Fx`] in its recording mode) hands every value it observed to, and what puts each one on the run's tape in the order the loop observed it.

use std::collections::VecDeque;
use std::convert::Infallible;
use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use aterm_types::MutexExt;
use std::time::{Duration, Instant};

use astream_effects::{EffectRecord, EffectSeam, RealEffects, RecordSink, RecordingSeam};

use super::codec::{self, CodecError, End, Item, Mark, Stamp};
use super::frame::{Hint, TapeWriter};
use super::header::{Roots, escape};
use crate::supervise::CtlReply;
use crate::supervise::policy::sleeps::{Boot, SleepStamp};

/// A note's kind: a turn-end decision's fold and reading, as `Debug`.
pub(crate) const NOTE_TURN_END: &str = "turn_end";

/// A note's kind: something happened that no replay of the tape can make (an older build's mail delivery received under a recorder), said once; this build writes none, and refuses to replay a tape that holds one.
pub(crate) const NOTE_UNREPLAYABLE: &str = "unreplayable";

/// A note's kind: the run UNWOUND INSIDE A DOOR'S LIVE CALL — a probe's body or the transport panicked between the request and its value, so the round trip has no record (there is no value to tape).
pub(crate) const NOTE_UNWOUND_IN: &str = "unwound_in";

/// [`NOTE_UNWOUND_IN`]'s text for a panic at `door` asking `request`.
pub(crate) fn unwound_in_text(door: &str, request: &[u8]) -> String {
    format!("{door}\n{}", aterm_codec::hex::encode_lower(request))
}

/// [`NOTE_UNWOUND_IN`]'s text read back: the door and the request, or `None` for a text [`unwound_in_text`] did not write.
pub(crate) fn unwound_in_of(text: &str) -> Option<(String, Vec<u8>)> {
    let (door, hex) = text.split_once('\n')?;
    Some((
        door.to_string(),
        aterm_codec::hex::decode_vec(hex.as_bytes())?,
    ))
}

/// How many screens the recorder remembers the records of
/// ([`TapeCore::screen_read`]): a decision judges a screen read in the
/// same look, or one carried from the look before.
const SCREENS: usize = 64;

/// A value a round trip returns, as a tape holds it: what a recorder writes ([`Self::to_tape`]) and what a replay or a reader of the tape reads back ([`Self::from_tape`]).
pub(crate) trait TapeValue: Sized {
    /// The value's bytes on the tape.
    fn to_tape(&self) -> Vec<u8>;

    /// The value [`Self::to_tape`] wrote.
    /// # Errors
    /// Bytes [`Self::to_tape`] did not write.
    fn from_tape(bytes: &[u8]) -> Result<Self, CodecError>;

    /// THE ANSWER AT A DOOR NEWER THAN THE TAPE (the occupancy red-team's F8): what a replay answers at a host door the tape's recording build did not have, as its header's `host_doors` names them (`header::Header::host_doors`), without taking a record.
    fn absent() -> Option<Self> {
        None
    }
}

/// A round trip with nothing to say back: a sleep, a bound.
impl TapeValue for () {
    fn to_tape(&self) -> Vec<u8> {
        Vec::new()
    }

    fn from_tape(bytes: &[u8]) -> Result<Self, CodecError> {
        if bytes.is_empty() {
            Ok(())
        } else {
            Err(CodecError {
                at: 0,
                why: "bytes in a reply that has none",
            })
        }
    }
}

/// A control request's reply, or the transport's error
/// ([`codec::ctl_reply`]).
impl TapeValue for Result<CtlReply, String> {
    fn to_tape(&self) -> Vec<u8> {
        codec::ctl_reply(self)
    }

    fn from_tape(bytes: &[u8]) -> Result<Self, CodecError> {
        codec::ctl_reply_of(bytes)
    }
}

/// A line written where the loop's own failure ends it (a printed line, a
/// notes line): nothing for done, else `1` and the error the loop ends on.
impl TapeValue for Result<(), String> {
    fn to_tape(&self) -> Vec<u8> {
        match self {
            Ok(()) => Vec::new(),
            Err(why) => [&[1u8][..], why.as_bytes()].concat(),
        }
    }

    fn from_tape(bytes: &[u8]) -> Result<Self, CodecError> {
        match bytes.split_first() {
            None => Ok(Ok(())),
            Some((1, why)) => String::from_utf8(why.to_vec())
                .map(Err)
                .map_err(|_| CodecError {
                    at: 1,
                    why: "an error that is not UTF-8",
                }),
            Some(_) => Err(CodecError {
                at: 0,
                why: "an outcome form this build does not know",
            }),
        }
    }
}

/// A write's outcome: nothing for done, else its error's kind
/// ([`codec::KINDS`]); the error's text is not kept (no write the loop
/// asks reads it).
impl TapeValue for io::Result<()> {
    fn to_tape(&self) -> Vec<u8> {
        match self {
            Ok(()) => Vec::new(),
            Err(e) => vec![codec::kind_code(e.kind())],
        }
    }

    fn from_tape(bytes: &[u8]) -> Result<Self, CodecError> {
        match bytes {
            [] => Ok(Ok(())),
            [code] => Ok(Err(io::Error::from(codec::kind_of(*code)))),
            _ => Err(CodecError {
                at: 1,
                why: "a write's outcome longer than its kind",
            }),
        }
    }
}

/// A derived door's number (a zone's offset in seconds): its decimal text.
impl TapeValue for i64 {
    fn to_tape(&self) -> Vec<u8> {
        self.to_string().into_bytes()
    }

    fn from_tape(bytes: &[u8]) -> Result<Self, CodecError> {
        std::str::from_utf8(bytes)
            .ok()
            .and_then(|t| t.parse().ok())
            .ok_or(CodecError {
                at: 0,
                why: "a number that does not read",
            })
    }
}

/// A derived door's number or none: its decimal text, nothing for none.
impl TapeValue for Option<i64> {
    fn to_tape(&self) -> Vec<u8> {
        self.map_or_else(Vec::new, |n| n.to_tape())
    }

    fn from_tape(bytes: &[u8]) -> Result<Self, CodecError> {
        if bytes.is_empty() {
            Ok(None)
        } else {
            i64::from_tape(bytes).map(Some)
        }
    }
}

/// The machine's count of its sleep ([`SleepStamp`]): its milliseconds and,
/// after a space, the boot's id in the kernel's spelling ([`Boot::text`]);
/// nothing for none.
impl TapeValue for Option<SleepStamp> {
    fn to_tape(&self) -> Vec<u8> {
        self.as_ref().map_or_else(Vec::new, |s| {
            let ms = u64::try_from(s.asleep.as_millis()).unwrap_or(u64::MAX);
            match &s.boot {
                Some(boot) => format!("{ms} {}", boot.text()),
                None => ms.to_string(),
            }
            .into_bytes()
        })
    }

    fn from_tape(bytes: &[u8]) -> Result<Self, CodecError> {
        if bytes.is_empty() {
            return Ok(None);
        }
        let bad = CodecError {
            at: 0,
            why: "a sleep count that does not read",
        };
        let text = std::str::from_utf8(bytes).map_err(|_| bad.clone())?;
        let (ms, boot) = match text.split_once(' ') {
            Some((ms, boot)) => (ms, Boot::of(boot)),
            None => (text, None),
        };
        let ms: u64 = ms.parse().map_err(|_| bad)?;
        Ok(Some(SleepStamp {
            asleep: Duration::from_millis(ms),
            boot,
        }))
    }
}

/// Two times or none (the machine's last sleep: when it slept and woke, unix
/// ms): their decimal texts, a space between; nothing for none.
impl TapeValue for Option<(i64, i64)> {
    fn to_tape(&self) -> Vec<u8> {
        self.map_or_else(Vec::new, |(a, b)| format!("{a} {b}").into_bytes())
    }

    fn from_tape(bytes: &[u8]) -> Result<Self, CodecError> {
        if bytes.is_empty() {
            return Ok(None);
        }
        std::str::from_utf8(bytes)
            .ok()
            .and_then(|t| t.split_once(' '))
            .and_then(|(a, b)| Some((a.parse().ok()?, b.parse().ok()?)))
            .map(Some)
            .ok_or(CodecError {
                at: 0,
                why: "two times that do not read",
            })
    }
}

/// A derived door's verdict (the kernel's word on a process): `1` or `0`.
impl TapeValue for bool {
    fn to_tape(&self) -> Vec<u8> {
        vec![if *self { b'1' } else { b'0' }]
    }

    fn from_tape(bytes: &[u8]) -> Result<Self, CodecError> {
        match bytes {
            b"1" => Ok(true),
            b"0" => Ok(false),
            _ => Err(CodecError {
                at: 0,
                why: "a verdict that is neither 1 nor 0",
            }),
        }
    }
}

/// The run's recorder ([module docs](self)).
pub(crate) struct TapeCore {
    recorder: Mutex<Recorder>,
    /// Live calls of doors in progress (the nesting rule).
    depth: AtomicUsize,
    /// The door whose live call is the outermost one in progress.
    outer: Mutex<String>,
    /// The monotonic instant the tape's `Mono` reads and stamps count from.
    origin: Instant,
    /// The run's roots: every path is taped under one ([`Roots::encode`]).
    roots: Mutex<Roots>,
    /// The seed of the worker's digest on this tape (an xxh64 of the tape's
    /// id): a policy probe names the worker by its digest under it
    /// (`WorkerEnv::digest`), so the same worker reads as another digest on
    /// another tape, and no digest is compared across tapes.
    seed: u64,
}

impl std::fmt::Debug for TapeCore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let rec = self.lock();
        let stage = rec.stage.lock_or_recover();
        f.debug_struct("TapeCore")
            .field("writer", &stage.writer)
            .field("effects", &stage.effects)
            .finish_non_exhaustive()
    }
}

/// The seam and what the recorder keeps beside it.
struct Recorder {
    seam: RecordingSeam<Handed, Streamed>,
    stage: Arc<Mutex<Stage>>,
    /// Each pair of nested doors already said ([`Mark::Nested`]).
    nested: Vec<(String, String)>,
    /// The last screens parsed, each with the record it came in.
    screens: Screens,
}

/// A screen the loop parsed: its sequence and generation, and the ordinal of
/// the round trip it came in.
#[derive(Debug)]
struct Screened {
    seq: u64,
    generation: Option<String>,
    record: u64,
}

/// The screens a run parsed, each with the ordinal of the round trip whose reply it came in: what a decision's mark names as the screen it judged.
#[derive(Debug, Default)]
pub(crate) struct Screens(VecDeque<Screened>);

impl Screens {
    /// The screen of `seq` and `generation` came in effect `#record`.
    pub(crate) fn push(&mut self, seq: u64, generation: Option<&str>, record: u64) {
        if self.0.len() == SCREENS {
            self.0.pop_front();
        }
        self.0.push_back(Screened {
            seq,
            generation: generation.map(str::to_string),
            record,
        });
    }

    /// The record the screen of `seq` and `generation` came in, the latest one first; `None` when it was never parsed (or is long forgotten).
    pub(crate) fn find(&self, seq: u64, generation: Option<&str>) -> Option<u64> {
        self.0
            .iter()
            .rev()
            .find(|s| s.seq == seq && s.generation.as_deref() == generation)
            .map(|s| s.record)
    }
}

/// What the seam's two ends share: the value staged for the seam to tape
/// (the seam draws a clock or a file from its backend, which here only hands
/// back what the door already read), the stamp the next round trip or flag
/// carries, and the writer the records stream into.
struct Stage {
    writer: TapeWriter,
    mono: u64,
    wall: u64,
    file: Option<io::Result<Vec<u8>>>,
    stamp: Option<Stamp>,
    /// Effects taped so far: the next one's ordinal (0-based, as
    /// [`Mark::Truncated`] counts them).
    effects: u64,
    /// The ordinal of the last round trip taped.
    last_exchange: Option<u64>,
}

/// The seam's backend: it reads nothing of its own, and hands the seam the
/// value the door staged.
struct Handed(Arc<Mutex<Stage>>);

impl RealEffects for Handed {
    fn real_now_nanos(&mut self) -> u64 {
        self.0.lock_or_recover().wall
    }

    fn real_now_mono_nanos(&mut self) -> u64 {
        self.0.lock_or_recover().mono
    }

    fn real_next_rand(&mut self) -> u64 {
        // The loop draws no random word; the door has no such read.
        0
    }

    fn real_read_file(&mut self, _path: &str) -> io::Result<Vec<u8>> {
        self.0
            .lock_or_recover()
            .file
            .take()
            .unwrap_or_else(|| Err(io::Error::other("no read was staged")))
    }
}

/// The seam's sink: each record, stamped where it carries a stamp, appended
/// to the writer.
struct Streamed(Arc<Mutex<Stage>>);

impl RecordSink for Streamed {
    fn record(&mut self, record: &EffectRecord) {
        let mut stage = self.0.lock_or_recover();
        let stamp = stage.stamp.take();
        let item = match (record, stamp) {
            (EffectRecord::Exchange { .. } | EffectRecord::Raised { .. }, Some(stamp)) => {
                Item::stamped(record.clone(), stamp)
            }
            _ => Item::effect(record.clone()),
        };
        if matches!(record, EffectRecord::Exchange { .. }) {
            stage.last_exchange = Some(stage.effects);
        }
        stage.effects += 1;
        stage.writer.append(&item);
    }
}

/// A seam call's value: the recording seam always answers.
fn answered<T>(r: Result<T, Infallible>) -> T {
    match r {
        Ok(v) => v,
        Err(never) => match never {},
    }
}

/// A door's live call in progress ([`TapeCore::enter`]); ended when dropped, an unwind included.
pub(crate) struct Entered<'a>(&'a TapeCore);

impl Drop for Entered<'_> {
    fn drop(&mut self) {
        self.0.depth.fetch_sub(1, Ordering::SeqCst);
    }
}

impl TapeCore {
    /// The recorder of a run whose tape `writer` writes (its header already written), its paths under `roots`, its monotonic reads counted from `origin` (the loop's clock as the run starts, peeked), the tape's id `tape` (its header's) seeding the worker's digest ([`Self::seed`]).
    pub(crate) fn start(
        writer: TapeWriter,
        roots: Roots,
        origin: Instant,
        tape: &str,
    ) -> Arc<Self> {
        let stage = Arc::new(Mutex::new(Stage {
            writer,
            mono: 0,
            wall: 0,
            file: None,
            stamp: None,
            effects: 0,
            last_exchange: None,
        }));
        let seam =
            RecordingSeam::with_sink(Handed(Arc::clone(&stage)), Streamed(Arc::clone(&stage)));
        Arc::new(Self {
            recorder: Mutex::new(Recorder {
                seam,
                stage,
                nested: Vec::new(),
                screens: Screens::default(),
            }),
            depth: AtomicUsize::new(0),
            outer: Mutex::new(String::new()),
            origin,
            roots: Mutex::new(roots),
            seed: super::header::digest_seed(tape),
        })
    }

    /// The seed of the worker's digest on this tape.
    pub(crate) const fn seed(&self) -> u64 {
        self.seed
    }

    /// The monotonic instant the tape counts from: what a host's answer that holds an `Instant` is written as an offset from ([`super::host::HostReply`]).
    pub(crate) const fn origin(&self) -> Instant {
        self.origin
    }

    fn lock(&self) -> MutexGuard<'_, Recorder> {
        self.recorder.lock_or_recover()
    }

    /// `at` in nanoseconds since the tape's origin (zero for an instant before it; the loop's clock never runs back).
    pub(crate) fn mono_ns(&self, at: Instant) -> u64 {
        u64::try_from(at.saturating_duration_since(self.origin).as_nanos()).unwrap_or(u64::MAX)
    }

    /// `at` as a signed offset from the tape's origin, `+<ns>ns` or `-<ns>ns`: how an `Instant` in a decision's outcome is written — and in a note ([`instants_as_offsets`]) — so a tape never holds this process's raw clock.
    pub(crate) fn offset_text(&self, at: Instant) -> String {
        offset_from(self.origin, at)
    }

    /// `path` as the run's roots write it (`@ledger/s-1.jsonl`); a byte that is not UTF-8 is replaced, a documented loss (the record's path is text).
    pub(crate) fn path(&self, path: &Path) -> String {
        String::from_utf8_lossy(&self.roots.lock_or_recover().encode(path)).into_owned()
    }

    /// The run's roots are `roots` from now on: a whole-run test helper's, whose run's header (and so its roots) is captured once its setup is done, after the door it records through was handed to the transport.
    #[cfg(test)]
    pub(crate) fn reroot(&self, roots: Roots) {
        *self.roots.lock_or_recover() = roots;
    }

    /// A live call of `door` begins: `None` when another door's live call is already in progress
    /// — the nesting rule: the caller runs live and tapes nothing.
    pub(crate) fn enter(&self, door: &str) -> Option<Entered<'_>> {
        if self.depth.fetch_add(1, Ordering::SeqCst) > 0 {
            self.depth.fetch_sub(1, Ordering::SeqCst);
            self.nested(door);
            return None;
        }
        door.clone_into(&mut self.outer.lock_or_recover());
        Some(Entered(self))
    }

    /// Whether a read of `what` comes inside a door's live call, said if so.
    fn inside(&self, what: &str) -> bool {
        if self.depth.load(Ordering::SeqCst) == 0 {
            return false;
        }
        self.nested(what);
        true
    }

    /// `inner` was reached inside the outer door's live call: said once a
    /// pair, and a failed assertion in this crate's tests.
    fn nested(&self, inner: &str) {
        let outer = self.outer.lock_or_recover().clone();
        {
            let mut rec = self.lock();
            if !rec.nested.iter().any(|(i, o)| *i == inner && *o == outer) {
                rec.nested.push((inner.to_string(), outer.clone()));
                rec.stage
                    .lock_or_recover()
                    .writer
                    .append(&Item::Mark(Mark::Nested {
                        inner: inner.to_string(),
                        outer: outer.clone(),
                    }));
            }
        }
        #[cfg(test)]
        panic!(
            "the door {inner:?} was reached inside the live call of {outer:?}: doors do not nest \
             (the replay plan's §4.4) — cross it outside the other door's live call"
        );
    }

    /// A monotonic read at `at`.
    pub(crate) fn mono(&self, at: Instant) {
        if self.inside("mono") {
            return;
        }
        let ns = self.mono_ns(at);
        let mut rec = self.lock();
        rec.stage.lock_or_recover().mono = ns;
        answered(rec.seam.mono_nanos());
    }

    /// A wall read of `ns` nanoseconds since the epoch (negative before it, written as its two's complement).
    pub(crate) fn clock(&self, ns: i64) {
        if self.inside("clock") {
            return;
        }
        let mut rec = self.lock();
        rec.stage.lock_or_recover().wall = ns as u64;
        answered(rec.seam.now_nanos());
    }

    /// A round trip that came back: `door`, its `request` and `reply`, stamped `stamp`.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "SupervisorRaisedLatch",
            action = "LoopOp",
            project = "aterm_agent::supervise::tape::conformance_tests::project_latch"
        )
    )]
    pub(crate) fn exchange(&self, door: &str, request: Vec<u8>, reply: Vec<u8>, stamp: Stamp) {
        let mut rec = self.lock();
        rec.stage.lock_or_recover().stamp = Some(stamp);
        let mut reply = Some(reply);
        let _ = answered(
            rec.seam
                .exchange(door, &request, &mut |_| reply.take().unwrap_or_default()),
        );
        rec.stage.lock_or_recover().stamp = None;
    }

    /// An output at `door`, its bytes built only here.
    pub(crate) fn emit(&self, door: &str, bytes: impl FnOnce() -> Vec<u8>) {
        if self.inside(door) {
            return;
        }
        let bytes = bytes();
        let mut rec = self.lock();
        answered(rec.seam.emit(door, &bytes));
    }

    /// A flag read at `door` that answered `up`: what the loop sees (true from its first true read on), the first true read taped with `stamp`.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "SupervisorRaisedLatch",
            action = "LoopOp",
            project = "aterm_agent::supervise::tape::conformance_tests::project_latch"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "SupervisorRaisedLatch",
            action = "Raise",
            reason = "Another thread storing the flag runs no tape code: the flag is an atomic the loop reads through its door. The Tier-1 bind's host thread raises it at each point of a recorded loop, and at moments of its own."
        )
    )]
    pub(crate) fn raised(&self, door: &str, up: bool, stamp: impl FnOnce() -> Stamp) -> bool {
        let mut rec = self.lock();
        if up {
            rec.stage.lock_or_recover().stamp = Some(stamp());
        }
        let seen = answered(rec.seam.raised(door, &mut || up));
        rec.stage.lock_or_recover().stamp = None;
        seen
    }

    /// A note of kind `kind`, its text built only here: a `Debug` rendering (a turn-end fold's state and reading), every `Instant` in it written as an offset from the tape's origin ([`instants_as_offsets`]).
    pub(crate) fn note(&self, kind: &str, text: impl FnOnce() -> String) {
        let item = Item::Note {
            kind: kind.to_string(),
            text: instants_as_offsets(&text(), self.origin),
        };
        self.lock().stage.lock_or_recover().writer.append(&item);
    }

    /// A file read at `path` that came back `read`: taped under the run's roots, and handed back to the door.
    pub(crate) fn file(&self, path: &Path, read: io::Result<Vec<u8>>) -> io::Result<Vec<u8>> {
        let path = self.path(path);
        let mut rec = self.lock();
        rec.stage.lock_or_recover().file = Some(read);
        answered(rec.seam.try_read_file(&path))
    }

    /// The live call of the door `door`, asking `request`, unwound ([`NOTE_UNWOUND_IN`]): said as it is, never rewritten (a request's hex holds no instant), and before the run's end the unwind goes on to.
    pub(crate) fn unwound_in(&self, door: &str, request: &[u8]) {
        self.lock()
            .stage
            .lock_or_recover()
            .writer
            .append(&Item::Note {
                kind: NOTE_UNWOUND_IN.to_string(),
                text: unwound_in_text(door, request),
            });
    }

    /// Close the open frame now, for `why`.
    pub(crate) fn flush(&self, why: Hint) {
        self.lock().stage.lock_or_recover().writer.flush_hint(why);
    }

    /// The loop parsed the screen of sequence `seq` and generation `generation` from the last
    /// round trip, so a decision's mark ([`Self::decide`]) names the record of that screen.
    pub(crate) fn screen_read(&self, seq: u64, generation: Option<&str>) {
        let mut rec = self.lock();
        let Some(record) = rec.stage.lock_or_recover().last_exchange else {
            return;
        };
        rec.screens.push(seq, generation, record);
    }

    /// A decision's mark at `door`: the record of the judged screen (by its `seq` and `generation`, `-` when the recorder never saw it parsed), the screen's own words, the `rule`, the `outcome` and the box's `facts`, as `key=value` lines ([`DecideMark`]).
    pub(crate) fn decide(
        &self,
        door: &str,
        seq: u64,
        generation: Option<&str>,
        rule: Option<&str>,
        outcome: &str,
        facts: Option<String>,
    ) {
        if self.inside(door) {
            return;
        }
        let mut rec = self.lock();
        let screen = rec.screens.find(seq, generation);
        let mark = DecideMark {
            screen,
            seq,
            generation: generation.map(str::to_string),
            rule: rule.map(str::to_string),
            outcome: outcome.to_string(),
            facts,
        };
        answered(rec.seam.emit(door, mark.encode().as_bytes()));
    }

    /// The run's end: said, and the tape made durable.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "SupervisorRaisedLatch",
            action = "EndRecord",
            project = "aterm_agent::supervise::tape::conformance_tests::project_latch"
        )
    )]
    pub(crate) fn end(&self, end: End) {
        self.lock()
            .stage
            .lock_or_recover()
            .writer
            .append(&Item::End(end));
    }

    /// Effects taped so far.
    #[cfg(test)]
    pub(crate) fn effects(&self) -> u64 {
        self.lock().stage.lock_or_recover().effects
    }
}

/// `at` as a signed offset from `origin`, `+<ns>ns` or `-<ns>ns` ([`TapeCore::offset_text`]; a replay's core writes the same instant of the replayed run the same way from its own origin).
pub(crate) fn offset_from(origin: Instant, at: Instant) -> String {
    match at.checked_duration_since(origin) {
        Some(after) => format!("+{}ns", after.as_nanos()),
        None => format!("-{}ns", origin.duration_since(at).as_nanos()),
    }
}

/// `text`, a `Debug` rendering, with every `Instant` in it written as an offset from `origin` — `+<ns>ns`, or `-<ns>ns` for one before it — as a decision's outcome writes one ([`TapeCore::offset_text`]).
pub(crate) fn instants_as_offsets(text: &str, origin: Instant) -> String {
    let Some(form) = InstantForm::learn(origin) else {
        return "(withheld: this build writes an Instant in a form the recorder cannot place on \
                the tape's clock)"
            .to_string();
    };
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(&form.head) {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        match form.read(rest) {
            Some((len, ns)) => {
                if let Some(off) = ns.checked_sub(form.origin_ns) {
                    if off < 0 {
                        out.push_str(&format!("{off}ns"));
                    } else {
                        out.push_str(&format!("+{off}ns"));
                    }
                } else {
                    out.push_str(&rest[..len]);
                }
                rest = &rest[len..];
            }
            None => {
                out.push_str(&form.head);
                rest = &rest[form.head.len()..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Whether `text`, a `Debug` rendering, holds an `Instant` as this build's std writes one ([`InstantForm`]): what a host's argument must never hold (`super::host::request`), since a raw clock in a compared request makes every replay diverge there.
#[cfg(test)]
pub(crate) fn holds_instant(text: &str) -> bool {
    let Some(form) = InstantForm::learn(Instant::now()) else {
        return false;
    };
    text.match_indices(&form.head)
        .any(|(at, _)| form.read(&text[at..]).is_some())
}

/// How this build's std writes an `Instant` in `Debug`
/// ([`instants_as_offsets`]): `head`, the seconds, `mid`, the nanoseconds,
/// `tail` — `Instant { tv_sec: `, `, tv_nsec: `, ` }` on unix today. On
/// Windows the instant is a `Duration` (`Instant { t: 12.5s }`): `mid` is the
/// decimal point and the nanoseconds a fraction whose trailing zeros are
/// trimmed, with no point at all on a whole second.
struct InstantForm {
    head: String,
    mid: String,
    tail: String,
    /// The origin, in the clock's own nanoseconds as its `Debug` wrote it.
    origin_ns: i128,
}

impl InstantForm {
    /// The form, from `origin`'s own `Debug`; `None` unless it has exactly
    /// that shape and an instant a known span on reads as that span.
    fn learn(origin: Instant) -> Option<Self> {
        const SPAN: Duration = Duration::new(1, 500_000_001);
        // The shape is read off the origin, or off the nanosecond after it
        // when the origin is a whole second, which a fraction form writes as
        // one number.
        let mut form = [Duration::ZERO, Duration::from_nanos(1)]
            .into_iter()
            .find_map(|nudge| Self::shape(&format!("{:?}", origin.checked_add(nudge)?)))?;
        let (_, origin_ns) = form.read(&format!("{origin:?}"))?;
        form.origin_ns = origin_ns;
        let later = format!("{:?}", origin.checked_add(SPAN)?);
        let (len, later_ns) = form.read(&later)?;
        (len == later.len()
            && later_ns.checked_sub(origin_ns) == Some(i128::try_from(SPAN.as_nanos()).ok()?))
        .then_some(form)
    }

    /// The form `written` (an instant's `Debug`) has: exactly two numbers,
    /// with text before each.
    fn shape(written: &str) -> Option<Self> {
        let mut runs = Vec::new();
        let bytes = written.as_bytes();
        let mut at = 0;
        while at < bytes.len() {
            if bytes[at].is_ascii_digit() {
                let start = at;
                while at < bytes.len() && bytes[at].is_ascii_digit() {
                    at += 1;
                }
                runs.push((start, at));
            } else {
                at += 1;
            }
        }
        let [(s0, e0), (s1, e1)] = runs[..] else {
            return None;
        };
        let form = Self {
            head: written[..s0].to_string(),
            mid: written[e0..s1].to_string(),
            tail: written[e1..].to_string(),
            origin_ns: 0,
        };
        (!form.head.is_empty() && !form.mid.is_empty()).then_some(form)
    }

    /// The instant `text` begins with in this form: its length in `text`
    /// and its nanoseconds on the clock.
    fn read(&self, text: &str) -> Option<(usize, i128)> {
        let rest = text.strip_prefix(&self.head)?;
        let (secs, rest) = number(rest)?;
        let (nanos, rest) = if self.mid == "." {
            match rest.strip_prefix('.') {
                Some(fraction) => {
                    let len = fraction.bytes().take_while(u8::is_ascii_digit).count();
                    if len == 0 || len > 9 {
                        return None;
                    }
                    let digits: i128 = fraction[..len].parse().ok()?;
                    let scale = 10_i128.pow(9 - u32::try_from(len).ok()?);
                    (digits * scale, &fraction[len..])
                }
                None => (0, rest),
            }
        } else {
            number(rest.strip_prefix(&self.mid)?)?
        };
        let rest = rest.strip_prefix(&self.tail)?;
        if !(0..1_000_000_000).contains(&nanos) {
            return None;
        }
        let ns = secs.checked_mul(1_000_000_000)?.checked_add(nanos)?;
        Some((text.len() - rest.len(), ns))
    }
}

/// The decimal number `text` begins with (a `-` before it allowed), and the
/// text after it.
fn number(text: &str) -> Option<(i128, &str)> {
    let digits = text.strip_prefix('-').unwrap_or(text);
    let len = digits.bytes().take_while(u8::is_ascii_digit).count();
    if len == 0 || len > 30 {
        return None;
    }
    let end = text.len() - digits.len() + len;
    Some((text[..end].parse().ok()?, &text[end..]))
}

/// A decision's mark, as its `Emit` holds it: `key=value` lines, each value escaped as the header's are ([`escape`]), a field the decision lacks written `-`, and the box's facts on a line of their own only where the decision has them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecideMark {
    /// The ordinal of the round trip whose reply was the judged screen.
    pub screen: Option<u64>,
    /// The judged screen's sequence.
    pub seq: u64,
    /// The judged screen's generation.
    pub generation: Option<String>,
    /// The rule that decided.
    pub rule: Option<String>,
    /// The decision, every `Instant` in it an offset from the tape's origin.
    pub outcome: String,
    /// What an approval or a question was decided on (`policy::box_facts::BoxFacts::to_json`): the facts its context was built from alone.
    pub facts: Option<String>,
}

impl DecideMark {
    /// The mark's text.
    #[must_use]
    pub fn encode(&self) -> String {
        let opt = |v: Option<&str>| v.map_or_else(|| "-".to_string(), |v| escape(v.as_bytes()));
        let mut out = format!(
            "screen={}\nseq={}\ngen={}\nrule={}\noutcome={}\n",
            self.screen
                .map_or_else(|| "-".to_string(), |n| n.to_string()),
            self.seq,
            opt(self.generation.as_deref()),
            opt(self.rule.as_deref()),
            escape(self.outcome.as_bytes()),
        );
        if let Some(facts) = &self.facts {
            out.push_str("facts=");
            out.push_str(&escape(facts.as_bytes()));
            out.push('\n');
        }
        out
    }

    /// A mark's text read back; `None` for text [`Self::encode`] did not write (a line that is no `key=value`, a field missing or unreadable).
    #[must_use]
    pub fn decode(text: &str) -> Option<Self> {
        let mut screen = None;
        let mut seq = None;
        let mut generation = None;
        let mut rule = None;
        let mut outcome = None;
        let mut facts = None;
        let text_of = |v: &str| {
            super::header::unescape(v)
                .ok()
                .and_then(|b| String::from_utf8(b).ok())
        };
        for line in text.lines() {
            let (key, value) = line.split_once('=')?;
            let opt = || (value != "-").then(|| text_of(value)).flatten();
            match key {
                "screen" => screen = Some((value != "-").then(|| value.parse().ok()).flatten()),
                "seq" => seq = value.parse().ok(),
                "gen" => generation = Some(opt()),
                "rule" => rule = Some(opt()),
                "outcome" => outcome = text_of(value),
                "facts" => facts = Some(text_of(value)?),
                _ => {}
            }
        }
        Some(Self {
            screen: screen?,
            seq: seq?,
            generation: generation?,
            rule: rule?,
            outcome: outcome?,
            facts,
        })
    }
}

#[cfg(test)]
#[path = "core_tests.rs"]
mod tests;
