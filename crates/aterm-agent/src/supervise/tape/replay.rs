// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! STRICT REPLAY: a harness tape run again, offline and exactly — the
//! recorded loop rebuilt from the tape's header and fed, door by door, the
//! values the recorded loop observed, every request and every output it
//! makes checked against the one on the tape where it occurs.
//!
//! **The core** ([`ReplayCore`]) is what a replaying run's door
//! (`supervise::fx::Fx` in its replay mode) asks instead of its live world.
//! It holds astream-effects' own replay seam ([`ReplaySeam`]) over the tape's
//! records — the counterpart of the recording seam the recorder taped them
//! through, so what a record MEANS on the way back (a monotonic read, a round
//! trip whose request must be the recorded one, an output that must be the
//! recorded bytes, a flag's first true read after so many false ones) is
//! astream's definition, not a second one kept here. The seam never calls a
//! live closure; neither does the core, which hands it closures that are
//! never called, and the door's replay arm never runs the live call it was
//! handed. Nothing a replay serves comes from anywhere but the tape: no
//! socket, no clock, no file, no process.
//!
//! **The first divergence ends the replay at once.** A request, an output,
//! a kind of read or a door that is not the recorded one, a tape that ends
//! under the loop, a reply this build cannot decode: the core latches the
//! FIRST such divergence — where it was (the effect's ordinal, `#N` in `tape
//! cat`), at which door, of which class ([`super::report::Class`]) — and
//! UNWINDS the loop from that very door (`resume_unwind` with a payload of
//! its own, which runs no panic hook). The replay plan's §5.2 had the core
//! poison every later door instead (each control request an `Err`, the
//! clocks standing still) and the loop find its own way out through its hard
//! failure path; that is not done, because after a divergence no door has a
//! right answer to give — a clock that stands still keeps a ride-out probing
//! for ever, a value of every other door's type would have to be invented —
//! and the report needs only the first divergence. Unwinding stops the loop
//! exactly at it, invents nothing, and needs no `?` threaded through the
//! loop. A destructor in the loop crosses no door (none does), so the unwind
//! meets none; a door crossed while one is under way (a future destructor
//! that did) cannot unwind a second time and ends the process, said first.
//!
//! **The driver** ([`replay`]) reads the tape (its text form too), decodes
//! its header with every root REMAPPED to a directory that does not exist
//! ([`super::header::Header::decode_remapped`]), builds the loop through
//! [`Session::from_setup`] over a control lane that answers from the tape
//! ([`super::ctl::ReplayCtl`]), runs the recorded entry under
//! `catch_unwind`, and says how it ended ([`super::report::Report`]). A
//! journal or ledger the run named is NOT opened (the loop's door says so:
//! `Fx::opens_files`): its rows are outputs, compared and never written.
//!
//! The loop this build replays is the window harness's hosted one, and in
//! this crate's tests the engine's `watch` and `supervise` seams over it; a
//! tape of the removed `aterm drive watch|supervise` reads (`tape cat`,
//! `tape show`) but does not replay, and its teller lane (`story`, a second
//! client) has no loop to cross.
//!
//! **A recorded panic** replays to PANICKED. One above every door (in the
//! policy, the loop): the replayed loop panics with the same message once
//! every record is consumed. One INSIDE a door's live call (a probe's body,
//! the transport) left no record of that round trip — there was no value to
//! tape — so the recorder says the door and the request instead
//! (`core::NOTE_UNWOUND_IN`), and the replay that asks that very request at
//! that door where the tape is over, every record before it reproduced, has
//! reached the place the recorded run died: PANICKED at that door. It cannot
//! panic there itself, for it never makes the live call. One inside a
//! NOTIFICATION to the window's host (`Fx::host_tell`) left the output —
//! said before the host was told — as the tape's last record, and the same
//! note: the replay whose tell of that very notification is that last
//! record has reached it, and is PANICKED there too.
//!
//! A run that had a WINDOW HOST (a hosted run's, or a test's watch given one)
//! is replayed with the replay's host in its place (`super::host::ReplayHost`):
//! every answer the tape's, every notification compared, no host asked.
//!
//! **A counterfactual** ([`Options::candidate`]): the same driver over the
//! acts cursor ([`super::acts`]) in place of astream's strict seam, the loop
//! rebuilt under the candidate policy; the report says the first divergent
//! act, or STALE ([`ReplayCore::report_acts`]). **A shadow's turn ends**
//! ([`replay_shadowed`], `super::shadow`): a strict replay in which the loop
//! decides each turn end again under the candidate beside its own decision.
//!
//! **A decision's trace** ([`replay_traced`], what `aterm drive replay --why`
//! asks of a turn end): with [`Options::trace_at`] the replayed loop decides
//! the turn end at that mark with its rule trace kept (`Fx::tracing`, true
//! only here), on the fold the replay rebuilt, and the core keeps the steps
//! the decision handed it when its mark is the one asked for. A running loop
//! never keeps one.

use std::any::Any;
use std::io;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use astream_effects::{EffectRecord, EffectSeam, EffectsLog, ReplayDivergence, ReplaySeam};
use aterm_types::MutexExt;

use super::acts::{ActsCursor, Candidate, Refusal};
use super::codec::{self, End, Item, Mark, Stamp};
use super::core::{
    DecideMark, NOTE_UNREPLAYABLE, NOTE_UNWOUND_IN, Screens, TapeValue, instants_as_offsets,
    offset_from, unwound_in_of,
};
use super::ctl::{ReplayCtl, is_act};
use super::header::{Entry, Header, Roots};
use super::host::ReplayHost;
use super::report::{
    ActSide, ActsReport, Cause, Class, Decided, Divergence, Mode, Report, Verdict,
};
use super::{doors, show};
use crate::supervise::fx::Fx;
use crate::supervise::policy::trace::{Outcome, TraceStep};
use crate::supervise::run::{IdleHost, Session, SuperviseOpts};
use crate::supervise::{CtlReply, SupervisorConfig};

/// How many flag reads in a row may consume no record before the replay is
/// called stuck (the plan's progress guard): far past any loop's honest run
/// of flag reads between two clock reads, which is a handful.
pub(crate) const PROGRESS_READS: u64 = 100_000;

/// How far ahead of this process's monotonic clock a replay's origin stands:
/// an `Instant` a recorded run placed before its tape's origin (an offset
/// subtracted from a read) lands after the replay's own boot, never under it.
const ORIGIN_AHEAD: Duration = Duration::from_secs(400 * 24 * 3600);

/// The payload a replay unwinds its loop with: never a panic's, so
/// `resume_unwind` runs no hook and the driver tells it from the recorded
/// run's own panic reproduced.
struct Halted;

/// A no-op at every record a replay consumes, `n` its ordinal (`#n` in `tape
/// cat`): a debugger's breakpoint target (`rust-lldb`: `b at_door`, with a
/// condition on `n`) for stopping a replay where a tape says.
#[inline(never)]
pub fn at_door(n: u64) {
    std::hint::black_box(n);
}

/// What the loop asked when a divergence was found, for its class.
enum Asked<'a> {
    /// A read of a kind astream names (`Mono`, `Clock`, `File`).
    Read,
    Exchange {
        door: &'a str,
        request: &'a [u8],
    },
    /// An output (a row, a mark, a notification): its door is the step's.
    Emit,
}

/// The replay's core ([module docs](self)).
pub(crate) struct ReplayCore {
    state: Mutex<Replayer>,
    /// The instant every `Mono` record counts from in this replay.
    origin: Instant,
    /// The run's roots, remapped: every file the loop opens is under one of
    /// these, and is asked as the recorder wrote it.
    roots: Roots,
    /// The run's roots as recorded: a path that is a VALUE rather than a file
    /// the loop opens — a screen's operand, the approval policy's home — is
    /// not remapped, and is asked under these, as the recorder wrote it.
    recorded: Roots,
    /// The seed of the worker's digest, the recording's ([`Self::seed`]).
    seed: u64,
    /// A COUNTERFACTUAL: the acts cursor serves in place of astream's strict
    /// seam ([`super::acts`]).
    acts: bool,
}

impl std::fmt::Debug for ReplayCore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let st = self.lock();
        f.debug_struct("ReplayCore")
            .field("acts", &self.acts)
            .field("consumed", &st.cursor.position())
            .field("effects", &st.effects)
            .field("diverged", &st.diverged)
            .finish_non_exhaustive()
    }
}

/// What serves the replayed loop: astream's strict seam, or the
/// counterfactual's acts cursor.
enum Cursor {
    Strict(ReplaySeam),
    Acts(Box<ActsCursor>),
}

impl Cursor {
    /// Records consumed (or, in acts mode, consumed or dropped).
    fn position(&self) -> usize {
        match self {
            Self::Strict(seam) => seam.position(),
            Self::Acts(c) => c.position(),
        }
    }

    /// The record under the cursor.
    fn peek(&mut self) -> Option<&EffectRecord> {
        match self {
            Self::Strict(seam) => seam.peek(),
            Self::Acts(c) => c.records().get(c.position()),
        }
    }
}

struct Replayer {
    cursor: Cursor,
    effects: usize,
    /// The first divergence, once found: nothing is served after it.
    diverged: Option<Divergence>,
    /// `--until door=N`: the replay stops once effect `#N` is consumed.
    until: Option<u64>,
    reached: Option<u64>,
    /// Flag reads in a row that consumed nothing (the progress guard).
    idle: u64,
    /// The last monotonic read (nanoseconds since the origin) and wall read
    /// (nanoseconds since the epoch): where a peek stands.
    mono_ns: u64,
    wall_ns: i64,
    /// The ordinal of the last round trip consumed, and the screens parsed
    /// from replies — kept exactly as the recorder keeps them, so a
    /// decision's mark names the record it named.
    last_exchange: Option<u64>,
    screens: Screens,
    requests: Vec<Vec<String>>,
    told: Vec<Vec<String>>,
    lines: Vec<String>,
    /// The round trip the loop asked where the tape was over (astream's
    /// `Exhausted`), its door and request: where a run that died inside a
    /// door's live call is reached again (`core::NOTE_UNWOUND_IN`).
    past_end: Option<(String, Vec<u8>)>,
    /// A counterfactual's refusal came where its tape was SPENT
    /// ([`ActsCursor::spent`]): no round trip or file read left at or past
    /// the cursor — on a tape with no end, everything on it taken.
    acts_spent: bool,
    /// The door and request the tape says the recorded run died inside
    /// (`core::NOTE_UNWOUND_IN`), when it says one ([`ReplayCore::died_inside`]).
    unwound_in: Option<(String, Vec<u8>)>,
    /// The notification the recorded run died telling its host, once the
    /// replay has told it as the tape's last record ([`ReplayCore::emit`]).
    died_in: Option<String>,
    /// The decision's mark whose RULE TRACE the replay is asked for (the
    /// ordinal of its record: `Options::trace_at`), the steps of the decision
    /// about to be marked, and the trace kept once that mark is reached.
    trace_at: Option<u64>,
    pending: Vec<TraceStep>,
    traced: Option<Vec<TraceStep>>,
    /// A counterfactual's: the record of the last screen the loop parsed,
    /// and the loop's decisions in the segment it stands in (`decided_in`,
    /// that segment's first record).
    last_screen: Option<u64>,
    decided: Vec<Decided>,
    decided_in: usize,
    /// A SHADOW replay's candidate policy (`super::shadow`), the candidate's
    /// decision on the turn end about to be marked, and each one kept with
    /// its mark's ordinal.
    shadow: Option<SupervisorConfig>,
    pending_shadow: Option<Decided>,
    shadowed: Vec<(u64, Decided)>,
    /// The host's doors the recording build had, as the tape's header names
    /// them (`Header::host_doors`; none named on a tape from before):
    /// [`ReplayCore::lacks_door`]. `None`, a core no header set (a test's),
    /// lacks no door.
    host_doors: Option<Vec<String>>,
}

impl ReplayCore {
    /// The core of a replay of `records`, the files the loop opens under
    /// `roots` (remapped) and the paths it was shown under `recorded` (as the
    /// header recorded them), the worker's digest seeded by the tape's id `tape` as the recording's
    /// was, stopping after effect `#until` when one is named.
    pub(crate) fn start(
        records: Vec<EffectRecord>,
        roots: Roots,
        recorded: Roots,
        tape: &str,
        until: Option<u64>,
    ) -> Arc<Self> {
        let effects = records.len();
        let cursor = Cursor::Strict(ReplaySeam::new(EffectsLog(records)));
        Self::with(cursor, effects, roots, recorded, tape, until)
    }

    /// The core of a COUNTERFACTUAL replay of `records`, each stamped as
    /// `stamps` says ([`super::acts`]); otherwise as [`Self::start`].
    pub(crate) fn start_acts(
        records: Vec<EffectRecord>,
        stamps: Vec<Option<Stamp>>,
        roots: Roots,
        recorded: Roots,
        tape: &str,
        until: Option<u64>,
    ) -> Arc<Self> {
        let effects = records.len();
        let cursor = Cursor::Acts(Box::new(ActsCursor::new(records, stamps)));
        Self::with(cursor, effects, roots, recorded, tape, until)
    }

    fn with(
        cursor: Cursor,
        effects: usize,
        roots: Roots,
        recorded: Roots,
        tape: &str,
        until: Option<u64>,
    ) -> Arc<Self> {
        let now = Instant::now();
        let acts = matches!(cursor, Cursor::Acts(_));
        Arc::new(Self {
            state: Mutex::new(Replayer {
                effects,
                cursor,
                diverged: None,
                until,
                reached: None,
                idle: 0,
                mono_ns: 0,
                wall_ns: 0,
                last_exchange: None,
                screens: Screens::default(),
                requests: Vec::new(),
                told: Vec::new(),
                lines: Vec::new(),
                past_end: None,
                acts_spent: false,
                unwound_in: None,
                died_in: None,
                trace_at: None,
                pending: Vec::new(),
                traced: None,
                last_screen: None,
                decided: Vec::new(),
                decided_in: 0,
                shadow: None,
                pending_shadow: None,
                shadowed: Vec::new(),
                host_doors: None,
            }),
            origin: now.checked_add(ORIGIN_AHEAD).unwrap_or(now),
            roots,
            recorded,
            seed: super::header::digest_seed(tape),
            acts,
        })
    }

    fn lock(&self) -> MutexGuard<'_, Replayer> {
        self.state.lock_or_recover()
    }

    /// Records left on the tape past what the replay consumed: a run that
    /// returned with one left did not reproduce the recorded run.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "SupervisorRaisedLatch",
            action = "ReplayEnd",
            project = "aterm_agent::supervise::tape::conformance_tests::project_latch"
        )
    )]
    pub(crate) fn left(&self) -> usize {
        let st = self.lock();
        st.effects.saturating_sub(st.cursor.position())
    }

    /// The first divergence, once latched.
    #[cfg(test)]
    pub(crate) fn diverged(&self) -> Option<Divergence> {
        self.lock().diverged.clone()
    }

    /// Effects consumed so far, and on the tape.
    #[cfg(test)]
    pub(crate) fn consumed(&self) -> (usize, usize) {
        let st = self.lock();
        (st.cursor.position(), st.effects)
    }

    /// The seed of the worker's digest on this tape: the recording's, so the
    /// replayed loop names the worker in each request as the recorded one did.
    pub(crate) fn seed(&self) -> u64 {
        self.seed
    }

    /// The instant this replay's `Mono` records count from: what a host's
    /// answer that held an `Instant` is placed from
    /// ([`super::host::HostReply`]), as the recorder wrote it from its own.
    pub(crate) fn origin(&self) -> Instant {
        self.origin
    }

    /// `path` as the recorder wrote it: under the remapped roots for a file
    /// the loop opens (the setup's paths, moved), else under the recorded
    /// roots — a value the loop was shown or given, never moved. No path the
    /// recorded run saw is under a remapped root: those directories do not
    /// exist, and are named for this replay alone.
    pub(crate) fn path(&self, path: &Path) -> String {
        let roots = if self.roots.holds(path) {
            &self.roots
        } else {
            &self.recorded
        };
        String::from_utf8_lossy(&roots.encode(path)).into_owned()
    }

    /// `at` as an offset from this replay's origin: what the recorder wrote
    /// for the same instant of the recorded run.
    pub(crate) fn offset_text(&self, at: Instant) -> String {
        offset_from(self.origin, at)
    }

    /// Where the replay's clocks stand, read without consuming a record: the
    /// last monotonic read and the last wall read, in Unix milliseconds.
    pub(crate) fn peek(&self) -> (Instant, i64) {
        let st = self.lock();
        if let Cursor::Acts(c) = &st.cursor {
            let (mono, wall) = c.now();
            return (
                self.origin + Duration::from_nanos(mono),
                wall.div_euclid(1_000_000),
            );
        }
        (
            self.origin + Duration::from_nanos(st.mono_ns),
            st.wall_ns.div_euclid(1_000_000),
        )
    }

    /// Take one record through `take`, which asks the seam at `door` for
    /// what the loop `asked`; a refusal is the replay's first divergence, and
    /// unwinds the loop from here.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "SuperviseReplayCursor",
            action = "AskNext",
            project = "aterm_agent::supervise::tape::conformance_tests::project_cursor"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "SuperviseReplayCursor",
            action = "AskOtherRead",
            project = "aterm_agent::supervise::tape::conformance_tests::project_cursor"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "SuperviseReplayCursor",
            action = "AskOtherAct",
            project = "aterm_agent::supervise::tape::conformance_tests::project_cursor"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "SuperviseReplayCursor",
            action = "AskPastEnd",
            project = "aterm_agent::supervise::tape::conformance_tests::project_cursor"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "SuperviseReplayCursor",
            action = "AskAfterDivergence",
            project = "aterm_agent::supervise::tape::conformance_tests::project_cursor"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "SuperviseReplayCursor",
            action = "AskRepeat",
            project = "aterm_agent::supervise::tape::conformance_tests::project_cursor"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "SuperviseReplayCursor",
            action = "AskClosingAct",
            project = "aterm_agent::supervise::tape::conformance_tests::project_cursor"
        )
    )]
    fn step<T>(
        &self,
        door: Option<&str>,
        asked: Asked<'_>,
        take: impl FnOnce(&mut ReplaySeam) -> Result<T, ReplayDivergence>,
    ) -> (T, u64) {
        let served = {
            let mut st = self.lock();
            if st.diverged.is_some() || st.reached.is_some() || st.died_in.is_some() {
                // Nothing is served after the first divergence (or the stop,
                // or the place the recorded run died).
                None
            } else {
                let at = st.cursor.position() as u64;
                let Cursor::Strict(seam) = &mut st.cursor else {
                    unreachable!("a strict step on a counterfactual's cursor")
                };
                match take(seam) {
                    Ok(value) => {
                        if st.cursor.position() as u64 > at {
                            st.idle = 0;
                            at_door(at);
                            if st.until == Some(at) {
                                st.reached = Some(at);
                            }
                        }
                        st.reached.is_none().then_some((value, at))
                    }
                    Err(refused) => {
                        if let (
                            Asked::Exchange { door, request },
                            ReplayDivergence::Exhausted { .. },
                        ) = (&asked, &refused)
                        {
                            st.past_end = Some(((*door).to_string(), request.to_vec()));
                        }
                        let class = class_of(&asked, st.cursor.peek(), door);
                        st.diverged = Some(Divergence {
                            at,
                            door: door.map(str::to_string),
                            class,
                            cause: Cause::Refused(refused),
                        });
                        None
                    }
                }
            }
        };
        match served {
            Some(served) => served,
            None => self.halt(),
        }
    }

    /// A COUNTERFACTUAL's step ([`super::acts`]): `take` asks the acts cursor
    /// at `door` for what the loop `asked`. A refusal — a read the segment
    /// never answered (STALE), an act it does not close with — is the
    /// replay's first divergence, and unwinds the loop from here; so does the
    /// progress guard, a loop served on and on with no record consumed.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "SuperviseReplayCursor",
            action = "AskNext",
            project = "aterm_agent::supervise::tape::conformance_tests::project_cursor"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "SuperviseReplayCursor",
            action = "AskOtherRead",
            project = "aterm_agent::supervise::tape::conformance_tests::project_cursor"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "SuperviseReplayCursor",
            action = "AskRepeat",
            project = "aterm_agent::supervise::tape::conformance_tests::project_cursor"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "SuperviseReplayCursor",
            action = "AskClosingAct",
            project = "aterm_agent::supervise::tape::conformance_tests::project_cursor"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "SuperviseReplayCursor",
            action = "AskOtherAct",
            project = "aterm_agent::supervise::tape::conformance_tests::project_cursor"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "SuperviseReplayCursor",
            action = "AskPastEnd",
            project = "aterm_agent::supervise::tape::conformance_tests::project_cursor"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "SuperviseReplayCursor",
            action = "AskAfterDivergence",
            project = "aterm_agent::supervise::tape::conformance_tests::project_cursor"
        )
    )]
    fn step_acts<T>(
        &self,
        door: Option<&str>,
        asked: Asked<'_>,
        take: impl FnOnce(&mut ActsCursor) -> Result<T, Refusal>,
    ) -> T {
        let served = {
            let mut st = self.lock();
            if st.diverged.is_some() || st.reached.is_some() || st.died_in.is_some() {
                // Nothing is served after the first divergence (or the stop,
                // or the place the recorded run died).
                None
            } else {
                let Cursor::Acts(c) = &mut st.cursor else {
                    unreachable!("a counterfactual's step on a strict seam")
                };
                let before = c.position();
                let result = take(c);
                let (after, idle) = (c.position(), c.idle);
                // What a refusal says, read off the cursor before it is let
                // go: the divergence, whether it came in the tape's last
                // segment (no act left), and whether the tape is spent there.
                let refused = result.as_ref().err().map(|refusal| {
                    let last_segment = c.closing().is_none();
                    (
                        acts_divergence(c, refusal, door, &asked),
                        last_segment,
                        c.spent(),
                    )
                });
                match (result, refused) {
                    (Ok(value), _) => {
                        if after > before {
                            at_door((after - 1) as u64);
                            if let Some(until) = st.until
                                && (before as u64..after as u64).contains(&until)
                            {
                                st.reached = Some(until);
                            }
                        }
                        if idle > PROGRESS_READS {
                            st.diverged = Some(Divergence {
                                at: after as u64,
                                door: door.map(str::to_string),
                                class: Class::Drift,
                                cause: Cause::NoProgress { reads: idle },
                            });
                            None
                        } else {
                            st.reached.is_none().then_some(value)
                        }
                    }
                    (Err(_), Some((divergence, last_segment, spent))) => {
                        // Asked in the last segment: where a run that died
                        // inside a door's live call is reached again.
                        if last_segment && let Asked::Exchange { door, request } = &asked {
                            st.past_end = Some(((*door).to_string(), request.to_vec()));
                        }
                        st.acts_spent = spent;
                        st.diverged = Some(divergence);
                        None
                    }
                    (Err(_), None) => None,
                }
            }
        };
        match served {
            Some(served) => served,
            None => self.halt(),
        }
    }

    /// Latch `d` as the first divergence (unless one already is) and unwind.
    fn diverge(&self, d: Divergence) -> ! {
        {
            let mut st = self.lock();
            if st.diverged.is_none() && st.reached.is_none() {
                st.diverged = Some(d);
            }
        }
        self.halt()
    }

    /// Unwind the loop from the door it is at ([module docs](self)).
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "SuperviseReplayCursor",
            action = "AskOtherRead",
            project = "aterm_agent::supervise::tape::conformance_tests::project_cursor"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "SuperviseReplayCursor",
            action = "AskOtherAct",
            project = "aterm_agent::supervise::tape::conformance_tests::project_cursor"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "SuperviseReplayCursor",
            action = "AskPastEnd",
            project = "aterm_agent::supervise::tape::conformance_tests::project_cursor"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "SuperviseReplayCursor",
            action = "AskAfterDivergence",
            project = "aterm_agent::supervise::tape::conformance_tests::project_cursor"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "SuperviseReplayCursor",
            action = "AskRepeat",
            project = "aterm_agent::supervise::tape::conformance_tests::project_cursor"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "SuperviseReplayCursor",
            action = "AskClosingAct",
            project = "aterm_agent::supervise::tape::conformance_tests::project_cursor"
        )
    )]
    fn halt(&self) -> ! {
        if std::thread::panicking() {
            eprintln!(
                "aterm drive replay: a door was crossed while the replay unwound from its \
                 divergence (a destructor crossed a door): the replay cannot go on"
            );
            aterm_uds::process::kill_self_now();
        }
        std::panic::resume_unwind(Box::new(Halted))
    }

    /// A monotonic read.
    pub(crate) fn mono(&self) -> Instant {
        if self.acts {
            let ns = self.step_acts(None, Asked::Read, |c| Ok(c.mono()));
            return self.origin + Duration::from_nanos(ns);
        }
        let (ns, _) = self.step(None, Asked::Read, |seam| seam.mono_nanos());
        self.lock().mono_ns = ns;
        self.origin + Duration::from_nanos(ns)
    }

    /// A wall read: nanoseconds since the epoch (negative before it).
    pub(crate) fn clock(&self) -> i64 {
        if self.acts {
            return self.step_acts(None, Asked::Read, |c| Ok(c.clock())) as i64;
        }
        let (ns, _) = self.step(None, Asked::Read, |seam| seam.now_nanos());
        let ns = ns as i64;
        self.lock().wall_ns = ns;
        ns
    }

    /// A round trip at `door` asking `request`: the recorded reply.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "SupervisorRaisedLatch",
            action = "ReplayOp",
            project = "aterm_agent::supervise::tape::conformance_tests::project_latch"
        )
    )]
    pub(crate) fn exchange(&self, door: &str, request: &[u8]) -> Vec<u8> {
        let asked = Asked::Exchange { door, request };
        let (reply, at) = if self.acts {
            let served = self.step_acts(Some(door), asked, |c| c.exchange(door, request));
            (served.reply, served.at.map(|at| at as u64))
        } else {
            let (reply, at) = self.step(Some(door), asked, |seam| {
                seam.exchange(door, request, &mut |_: &[u8]| Vec::new())
            });
            (reply, Some(at))
        };
        let mut st = self.lock();
        st.last_exchange = at;
        if door == doors::CTL || door == doors::TELLER {
            let words = codec::ctl_args(request).unwrap_or_default();
            if door == doors::CTL {
                st.requests.push(words);
            } else {
                st.told.push(words);
            }
        } else if door == doors::OUT {
            st.lines.push(String::from_utf8_lossy(request).into_owned());
        }
        reply
    }

    /// The recorded reply at `door`, effect `#at`, does not decode as the
    /// value the loop asked for: the replay's divergence.
    pub(crate) fn undecodable(&self, door: &str, why: &str) -> ! {
        let at = self.lock().last_exchange.unwrap_or(0);
        self.diverge(Divergence {
            at,
            door: Some(door.to_string()),
            class: Class::Drift,
            cause: Cause::Undecodable {
                door: door.to_string(),
                why: why.to_string(),
            },
        })
    }

    /// The tape says the recorded run died inside `door`'s live call asking
    /// `request` (`core::NOTE_UNWOUND_IN`): what [`Self::emit`] knows the
    /// place by when the door is a notification to the window's host.
    pub(crate) fn died_inside(&self, door: String, request: Vec<u8>) {
        self.lock().unwound_in = Some((door, request));
    }

    /// An output at `door`: compared with the recorded one — never, in a
    /// counterfactual, where the record under the cursor is taken when it is
    /// one of that door ([`super::acts`]).
    ///
    /// The output that is the tape's LAST record, where the tape says the
    /// recorded run died inside that door asking those very bytes, is a
    /// notification the run died telling its host (`Fx::host_tell`): the
    /// replay has reached the place the recorded run died — which it cannot
    /// die at itself, for it tells no host — and stops there, as it stops at
    /// a query asked again past the tape's end (the driver's PANICKED).
    pub(crate) fn emit(&self, door: &str, bytes: &[u8]) {
        // The step returned, so the output was the recorded one (or, in a
        // counterfactual, was not compared), and no divergence or stop is
        // latched.
        if self.acts {
            self.step_acts(Some(door), Asked::Emit, |c| Ok(c.emit(door)));
        } else {
            self.step(Some(door), Asked::Emit, |seam| seam.emit(door, bytes));
        }
        let died = {
            let mut st = self.lock();
            let last = st.cursor.position() >= st.effects;
            let named = st
                .unwound_in
                .as_ref()
                .is_some_and(|(d, request)| d == door && request.as_slice() == bytes);
            if last && named {
                st.died_in = Some(door.to_string());
            }
            last && named
        };
        if died {
            self.halt();
        }
    }

    /// A flag read at `door`: true from its recorded first true read on (in
    /// a counterfactual, once the cursor has passed it: [`super::acts`]).
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "SupervisorRaisedLatch",
            action = "ReplayOp",
            project = "aterm_agent::supervise::tape::conformance_tests::project_latch"
        )
    )]
    pub(crate) fn raised(&self, door: &str) -> bool {
        if self.acts {
            return self.step_acts(Some(door), Asked::Read, |c| Ok(c.raised(door)));
        }
        let (up, at) = self.step(Some(door), Asked::Read, |seam| {
            seam.raised(door, &mut || false)
        });
        let mut st = self.lock();
        if st.cursor.position() as u64 > at {
            return up;
        }
        st.idle += 1;
        if st.idle <= PROGRESS_READS {
            return up;
        }
        let reads = st.idle;
        drop(st);
        self.diverge(Divergence {
            at,
            door: Some(door.to_string()),
            class: Class::Drift,
            cause: Cause::NoProgress { reads },
        })
    }

    /// A file read at `path`: the recorded bytes or error kind.
    pub(crate) fn file(&self, path: &Path) -> io::Result<Vec<u8>> {
        let path = self.path(path);
        if self.acts {
            let asked = Asked::Exchange {
                door: "file",
                request: path.as_bytes(),
            };
            return self.step_acts(None, asked, |c| c.file(&path));
        }
        self.step(None, Asked::Read, |seam| seam.try_read_file(&path))
            .0
    }

    /// The loop parsed the screen of `seq` and `generation` from the last
    /// round trip.
    pub(crate) fn screen_read(&self, seq: u64, generation: Option<&str>) {
        let mut st = self.lock();
        if let Some(record) = st.last_exchange {
            st.screens.push(seq, generation, record);
            st.last_screen = Some(record);
        }
    }

    /// Whether this replay keeps the trace of a decision ([`Options::trace_at`]).
    pub(crate) fn tracing(&self) -> bool {
        self.lock().trace_at.is_some()
    }

    /// The trace of the decision about to be marked (`Fx::traced`, right
    /// before its mark): kept until that mark,
    /// every `Instant` in what each says written as an offset from this
    /// replay's origin, as a tape writes every instant.
    pub(crate) fn traced(&self, steps: Vec<TraceStep>) {
        let origin = self.origin;
        self.lock().pending.extend(steps.into_iter().map(|mut s| {
            s.outcome = match s.outcome {
                Outcome::Decided(t) => Outcome::Decided(instants_as_offsets(&t, origin)),
                Outcome::Overruled(t) => Outcome::Overruled(instants_as_offsets(&t, origin)),
                Outcome::Unproven(t) => Outcome::Unproven(instants_as_offsets(&t, origin)),
                Outcome::Passed => Outcome::Passed,
            };
            s
        }));
    }

    /// The trace kept at the mark the replay was asked for, once reached.
    fn trace(&self) -> Option<Vec<TraceStep>> {
        self.lock().traced.clone()
    }

    /// Keep the trace of the decision whose mark is effect `#n`.
    fn trace_at(&self, n: u64) {
        self.lock().trace_at = Some(n);
    }

    /// The host's doors the recording build had (`Header::host_doors`; an
    /// empty set for a tape from before headers named them).
    pub(crate) fn recorded_doors(&self, doors: Vec<String>) {
        self.lock().host_doors = Some(doors);
    }

    /// Whether the recording build had no host door `door`, as its header
    /// names its doors ([`Self::recorded_doors`]): where the answer's type
    /// has one for that case (`TapeValue::absent`), the replay gives it and
    /// takes no record (`Fx::host_exchange`). A core no header set lacks
    /// none.
    pub(crate) fn lacks_door(&self, door: &str) -> bool {
        self.lock()
            .host_doors
            .as_ref()
            .is_some_and(|doors| !doors.iter().any(|d| d == door))
    }

    /// The candidate policy this SHADOW replay decides each turn end again
    /// under, if it is one (`Fx::shadow_policy`).
    pub(crate) fn shadow_policy(&self) -> Option<SupervisorConfig> {
        self.lock().shadow.clone()
    }

    /// The candidate's decision on the turn end about to be marked
    /// (`Fx::shadowed`): kept with that mark.
    pub(crate) fn shadowed(&self, rule: Option<&str>, outcome: String) {
        self.lock().pending_shadow = Some(Decided {
            door: String::new(),
            rule: rule.map(str::to_string),
            outcome,
        });
    }

    /// A decision's mark at `door`: built as the recorder built it, and
    /// compared with the recorded one (a counterfactual keeps the last one
    /// for its report, and compares none).
    pub(crate) fn decide(
        &self,
        door: &str,
        seq: u64,
        generation: Option<&str>,
        rule: Option<&str>,
        outcome: &str,
        facts: Option<String>,
    ) {
        let screen = {
            // The steps the decision handed in are this mark's: kept where it
            // is the mark asked for (the record its emit is about to
            // consume), dropped otherwise.
            let mut st = self.lock();
            let pending = std::mem::take(&mut st.pending);
            if st.trace_at == Some(st.cursor.position() as u64) {
                st.traced = Some(pending);
            }
            // A shadow's decision on this point, kept with the mark's ordinal
            // (the record its emit is about to consume).
            if let Some(mut candidate) = st.pending_shadow.take() {
                candidate.door = door.to_string();
                let at = st.cursor.position() as u64;
                st.shadowed.push((at, candidate));
            }
            if let Cursor::Acts(c) = &st.cursor {
                let segment = c.segment_start();
                if st.decided_in != segment {
                    st.decided.clear();
                    st.decided_in = segment;
                }
                st.decided.push(Decided {
                    door: door.to_string(),
                    rule: rule.map(str::to_string),
                    outcome: outcome.to_string(),
                });
            }
            st.screens.find(seq, generation)
        };
        let mark = DecideMark {
            screen,
            seq,
            generation: generation.map(str::to_string),
            rule: rule.map(str::to_string),
            outcome: outcome.to_string(),
            facts,
        };
        self.emit(door, mark.encode().as_bytes());
    }
}

/// A counterfactual's divergence for `refusal` at `door`, the loop having
/// `asked` what it asked, read off the cursor `c` where it refused.
fn acts_divergence(
    c: &ActsCursor,
    refusal: &Refusal,
    door: Option<&str>,
    asked: &Asked<'_>,
) -> Divergence {
    let at = c.position() as u64;
    let asked_side = match asked {
        Asked::Exchange { door, request } => Some(act_side(None, door, request)),
        Asked::Read | Asked::Emit => None,
    };
    // The act the tape holds at `n`, as a report names it.
    let act_at = |n: usize| match c.records().get(n) {
        Some(EffectRecord::Exchange { door, request, .. }) => {
            Some(act_side(Some(n as u64), door, request))
        }
        _ => None,
    };
    match refusal {
        Refusal::Skipped { closing, .. } => {
            let recorded = act_at(*closing).unwrap_or_else(|| ActSide {
                at: Some(*closing as u64),
                door: String::new(),
                words: String::new(),
            });
            Divergence {
                at: *closing as u64,
                door: Some(recorded.door.clone()),
                class: Class::Decision,
                cause: Cause::ActSkipped {
                    recorded,
                    asked: asked_side.unwrap_or_else(|| ActSide {
                        at: None,
                        door: door.unwrap_or_default().to_string(),
                        words: String::new(),
                    }),
                },
            }
        }
        Refusal::Stale { class } => Divergence {
            at,
            door: door.map(str::to_string),
            class: Class::Drift,
            cause: Cause::Stale {
                class: class.word(),
                request: asked_side.map_or_else(String::new, |s| s.words),
            },
        },
        Refusal::ActDiffers { closing } => {
            let recorded = closing.and_then(act_at);
            Divergence {
                at: closing.map_or(at, |n| n as u64),
                door: door.map(str::to_string),
                class: Class::Decision,
                cause: Cause::ActDiffers {
                    recorded,
                    asked: asked_side,
                },
            }
        }
    }
}

/// An act (or any round trip) as a report names it: a control request's
/// words, a host call's arguments as text.
fn act_side(at: Option<u64>, door: &str, request: &[u8]) -> ActSide {
    let words = if door == doors::CTL || door == doors::TELLER {
        codec::ctl_args(request).map_or_else(
            |_| String::from_utf8_lossy(request).into_owned(),
            |w| w.join(" "),
        )
    } else {
        String::from_utf8_lossy(request).into_owned()
    };
    ActSide {
        at,
        door: door.to_string(),
        words,
    }
}

/// Whether `request` on a control lane is an act ([`is_act`]).
fn is_act_request(request: &[u8]) -> bool {
    codec::ctl_args(request).is_ok_and(|words| {
        let words: Vec<&str> = words.iter().map(String::as_str).collect();
        is_act(&words)
    })
}

/// A door whose crossing is an output or a write the loop made.
fn is_output(door: &str) -> bool {
    [
        doors::OUT,
        doors::NOTES,
        doors::JOURNAL,
        doors::LEDGER,
        doors::WRITE_BOUND,
        doors::TASK_RESULT,
    ]
    .contains(&door)
}

/// The class of a divergence ([`super::report::Class`]): the loop `asked`,
/// at `door`, where the tape holds `found`.
fn class_of(asked: &Asked<'_>, found: Option<&EffectRecord>, door: Option<&str>) -> Class {
    let lane = |d: &str| d == doors::CTL || d == doors::TELLER;
    // An act: a control request that writes into the session, or one of the
    // host's acts (its step at an idle point, a restart, a reset, a probe).
    let act = |door: &str, request: &[u8]| {
        (lane(door) && is_act_request(request)) || doors::is_host_act(door)
    };
    let act_asked = matches!(asked, Asked::Exchange { door, request } if act(door, request));
    let found_door = match found {
        Some(EffectRecord::Exchange { door, .. } | EffectRecord::Emit { door, .. }) => {
            Some(door.as_str())
        }
        _ => None,
    };
    let act_found = matches!(
        found,
        Some(EffectRecord::Exchange { door, request, .. }) if act(door, request)
    );
    let decide = |d: Option<&str>| d.is_some_and(|d| d.starts_with("decide."));
    if act_asked || act_found || decide(door) || decide(found_door) {
        return Class::Decision;
    }
    // Every emit is an output the loop produced — a journal row, a ledger
    // row, a notification it told its host — and so are the round trips that
    // write ([`is_output`]).
    let asked_output = match asked {
        Asked::Exchange { door, .. } => is_output(door),
        Asked::Emit => true,
        Asked::Read => false,
    };
    if asked_output && door.is_some() && door == found_door {
        return Class::Output;
    }
    Class::Drift
}

/// What a replay is asked to do beside replaying.
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// Stop after effect `#until` is consumed (`--until door=N`).
    pub until: Option<u64>,
    /// The directory every root is remapped under, which must not exist; by
    /// default `<tmp>/aterm-replay-<tape>/absent`.
    pub base: Option<PathBuf>,
    /// Keep the RULE TRACE of the decision whose mark is effect `#trace_at`
    /// ([`replay_traced`]): the turn-end decision made again on the fold the
    /// replay rebuilt, its every check said (`aterm drive replay --why` at a
    /// turn end, which stops the replay there with [`Self::until`]).
    pub trace_at: Option<u64>,
    /// A COUNTERFACTUAL: replay in ACTS MODE ([`super::acts`]) under this
    /// candidate policy (`--set k=v`, `--policy FILE`), up to the first act
    /// it takes otherwise. `None`: a strict replay.
    pub candidate: Option<Candidate>,
}

/// How a `watch` run's end is said on its tape (`drive_cli`'s recorder and
/// the tests' alike): its exit code.
#[must_use]
pub fn watch_end(code: u8) -> String {
    format!("exit {code}")
}

/// How a `supervise` run's end is said on its tape (the engine tests' seam,
/// an older build's `aterm drive supervise`): its exit code, then the review
/// it returns for the manager (the phase lines, the box or the last rows, a
/// `TIMEOUT` before them), line for line. That text is the run's main OUTPUT: a `watch`'s
/// lines cross the `out` door one by one where they are printed, but a
/// `supervise` returns its review as its value, so the end is where it is
/// compared — a build that renders the review otherwise, on the very same
/// screens and requests, does not reproduce the recorded run.
#[must_use]
pub fn supervise_end(end: &Result<(String, u8), String>) -> String {
    match end {
        Ok((text, code)) => format!("exit {code}\n{text}"),
        Err(e) => format!("error: {e}"),
    }
}

/// How a hosted run's end is said on its tape.
#[must_use]
pub fn hosted_end(end: &Result<(), String>) -> String {
    match end {
        Ok(()) => "returned".to_string(),
        Err(e) => format!("error: {e}"),
    }
}

/// A panic's message, as a tape's end says it.
#[must_use]
pub fn panic_message(panic: &(dyn Any + Send)) -> String {
    panic
        .downcast_ref::<&str>()
        .map(|s| (*s).to_string())
        .or_else(|| panic.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "a panic with no message".to_string())
}

/// REPLAY the tape `bytes` (its file form or its text form) strictly
/// ([module docs](self)).
///
/// # Errors
///
/// A tape that cannot be replayed at all — unreadable or damaged inside, no
/// header or one this build cannot read, a loop this build does not replay,
/// a run that recorded something no replay can make — named; `aterm drive
/// replay` exits 2.
pub fn replay(bytes: &[u8], options: &Options) -> Result<Report, String> {
    replay_traced(bytes, options).map(|(report, _)| report)
}

/// [`replay`], and the RULE TRACE of the decision at [`Options::trace_at`]
/// when one is asked for and the replay reached its mark: the decision made
/// again, on the very state the replay rebuilt, with every check it
/// consulted said (`policy::trace`). Only a turn end's decision hands the
/// replay a trace (an approval is traced from its mark's facts, with no
/// replay: `tape::explain`).
///
/// # Errors
///
/// As [`replay`].
pub fn replay_traced(
    bytes: &[u8],
    options: &Options,
) -> Result<(Report, Option<Vec<TraceStep>>), String> {
    run_replay(bytes, options, None).map(|(report, core)| (report, core.trace()))
}

/// A STRICT replay that decides each turn end again under `candidate` beside
/// the loop's own decision (`super::shadow`): its report, and the
/// candidate's decision at each turn end's mark the replay reached, by the
/// mark's ordinal.
///
/// # Errors
///
/// As [`replay`].
pub(crate) fn replay_shadowed(
    bytes: &[u8],
    candidate: SupervisorConfig,
) -> Result<(Report, Vec<(u64, Decided)>), String> {
    run_replay(bytes, &Options::default(), Some(candidate)).map(|(report, core)| {
        let rows = std::mem::take(&mut core.lock().shadowed);
        (report, rows)
    })
}

/// [`replay_traced`]'s body: the report, and the core it ran on (its trace,
/// its shadow's rows), a shadow's candidate given to the core before the
/// loop runs.
fn run_replay(
    bytes: &[u8],
    options: &Options,
    shadow: Option<SupervisorConfig>,
) -> Result<(Report, Arc<ReplayCore>), String> {
    let tape = show::open(bytes, false)?;
    let text = tape
        .header
        .as_deref()
        .ok_or("the tape has no header: there is no loop to rebuild")?;
    let recorded = Header::decode(text)?;
    let id = recorded.tape.clone();
    let base = options.base.clone().unwrap_or_else(|| {
        std::env::temp_dir()
            .join(format!("aterm-replay-{}", sanitized(&id)))
            .join("absent")
    });
    if base.exists() {
        return Err(format!(
            "the replay's roots go under {}, which must not exist: a file there would be \
             opened in place of the recorded run's",
            base.display()
        ));
    }
    let mut header = Header::decode_remapped(text, &base)?;
    // `aterm drive watch|supervise` are gone: their tapes read (`tape cat`,
    // `tape show`), and only this crate's tests, which run those loops still,
    // replay them.
    if !cfg!(test) && header.entry != Entry::Hosted {
        return Err(format!(
            "a tape of `aterm drive {}`, which this build no longer has: only the window \
             harness's hosted runs replay",
            header.entry.word()
        ));
    }
    let mut tail = show::tail_note(&tape);
    // The items are MOVED into what the replay keeps — the records into the
    // seam, the end, the cap and the notes it reads beside them — never
    // copied: the decoded tape (up to the reader's held-item bound,
    // `recover::HELD_MAX`) is held once. A counterfactual keeps each
    // record's stamp beside it: its virtual clock runs on them.
    let acts = options.candidate.is_some();
    let (mut records, mut stamps) = (Vec::new(), Vec::new());
    let (mut end, mut capped, mut unwound_in) = (None, None, None);
    for item in tape.items {
        match item {
            Item::Effect { record, stamp } => {
                records.push(record);
                if acts {
                    stamps.push(stamp);
                }
            }
            Item::End(e) => end = end.or(Some(e)),
            Item::Mark(Mark::Truncated { door }) => capped = capped.or(Some(door)),
            Item::Note { kind, text } if kind == NOTE_UNREPLAYABLE => {
                return Err(format!(
                    "the recorded run did what no replay can make: {text}"
                ));
            }
            Item::Note { kind, text } if kind == NOTE_UNWOUND_IN => {
                unwound_in = unwound_in.or_else(|| unwound_in_of(&text));
            }
            Item::Note { .. } | Item::Mark(_) => {}
        }
    }
    // A counterfactual runs the recorded loop under the candidate's policy.
    if let Some(candidate) = &options.candidate {
        header.opts.policy = candidate.policy(&header.opts.policy)?;
    }
    let core = if acts {
        ReplayCore::start_acts(
            records,
            stamps,
            header.roots.clone(),
            recorded.roots,
            &header.tape,
            options.until,
        )
    } else {
        ReplayCore::start(
            records,
            header.roots.clone(),
            recorded.roots,
            &header.tape,
            options.until,
        )
    };
    if let Some((door, request)) = unwound_in.clone() {
        core.died_inside(door, request);
    }
    if let Some(n) = options.trace_at {
        core.trace_at(n);
    }
    core.lock().shadow = shadow;
    core.recorded_doors(header.host_doors.clone().unwrap_or_default());
    let fx = Fx::replay(Arc::clone(&core));
    let run = run_entry(&header, &fx);
    if let Some(door) = capped {
        tail = Some(format!(
            "the run reached its cap: effect #{door} on are not on the tape"
        ));
    }
    let report = match &options.candidate {
        Some(candidate) => {
            core.report_acts(&header, run, end.as_ref(), unwound_in, tail, candidate)
        }
        None => core.report(&header, run, end.as_ref(), unwound_in, tail),
    };
    Ok((report, core))
}

/// A tape's id as a path's word: what it may hold besides letters and digits
/// becomes `_` (an id is sixteen hex digits; a hand-made header's need not be).
fn sanitized(id: &str) -> String {
    id.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

/// Run the recorded entry over `fx`, under `catch_unwind`: how it ended, as a
/// tape says an end, or what it unwound with.
fn run_entry(header: &Header, fx: &Fx) -> std::thread::Result<String> {
    let mut ctl = ReplayCtl::new(fx.clone(), doors::CTL);
    let mut session = Session::from_setup(&mut ctl, header.setup.clone());
    session.set_fx(fx.clone());
    // A run that had a window host is replayed with the replay's host in its
    // place (`super::host::ReplayHost`): every answer the tape's, no host
    // asked. The loop hands its door the host it was given (`Fx::host_door`),
    // which on a replay is the replay's whatever it was handed.
    let opts = SuperviseOpts {
        idle_host: header
            .idle_host
            .then(|| Arc::new(ReplayHost::new(fx.clone())) as Arc<dyn IdleHost>),
        ..header.opts.clone()
    };
    let entry = header.entry;
    std::panic::catch_unwind(AssertUnwindSafe(|| match entry {
        // The engine tests' seams over the hosted loop's engine
        // (`Session::watch`, `Session::supervise`): a shipped build refuses
        // these tapes before it gets here ([`replay`]).
        #[cfg(test)]
        Entry::Watch => watch_end(session.watch(&opts, &mut io::sink())),
        #[cfg(test)]
        Entry::Supervise => supervise_end(&session.supervise(&opts)),
        #[cfg(not(test))]
        Entry::Watch | Entry::Supervise => {
            format!("error: no {} loop in this build", entry.word())
        }
        Entry::Hosted => hosted_end(&session.run_hosted(
            &opts,
            Arc::new(AtomicBool::new(false)),
            &mut io::sink(),
        )),
    }))
}

impl ReplayCore {
    /// The report of a replay whose run ended `run`, the tape's own end
    /// `end` (none for a tape cut short), the door and request the recorded
    /// run died inside when the tape says one (`core::NOTE_UNWOUND_IN`),
    /// `tail` how its bytes ended.
    fn report(
        &self,
        header: &Header,
        run: std::thread::Result<String>,
        end: Option<&End>,
        unwound_in: Option<(String, Vec<u8>)>,
        tail: Option<String>,
    ) -> Report {
        let left = self.left();
        let mut st = self.lock();
        let at = st.cursor.position() as u64;
        let found = st.cursor.peek().cloned();
        let next_door = match &found {
            Some(
                EffectRecord::Exchange { door, .. }
                | EffectRecord::Emit { door, .. }
                | EffectRecord::Raised { door, .. },
            ) => Some(door.clone()),
            _ => None,
        };
        let recorded_end = |end: Option<&End>| match end {
            Some(End::Returned { how }) => how.clone(),
            Some(End::Suspended) => "suspended".to_string(),
            Some(End::Unwound { message }) => format!("unwound: {message}"),
            None => "(no end on the tape)".to_string(),
        };
        let mut returned = None;
        let verdict = match run {
            Ok(how) => {
                returned = Some(how.clone());
                if left > 0 {
                    let class = class_of(&Asked::Read, found.as_ref(), None);
                    Verdict::Diverged(Divergence {
                        at,
                        door: next_door,
                        class,
                        cause: Cause::EndedEarly { left },
                    })
                } else {
                    match end {
                        Some(End::Returned { how: recorded }) if *recorded == how => {
                            Verdict::Reproduced
                        }
                        // A hosted run a seamless update's Commit stopped: its
                        // loop returned as from any stop, and the store said
                        // why (`store::run_hosted_taped`).
                        Some(End::Suspended)
                            if header.entry == Entry::Hosted && how == hosted_end(&Ok(())) =>
                        {
                            Verdict::Reproduced
                        }
                        None => Verdict::Ended,
                        other => Verdict::Diverged(Divergence {
                            at,
                            door: None,
                            class: Class::Output,
                            cause: Cause::OtherEnd {
                                recorded: recorded_end(other),
                                replayed: how,
                            },
                        }),
                    }
                }
            }
            Err(payload) if payload.is::<Halted>() => match (&st.reached, &st.diverged) {
                (Some(door), _) => Verdict::Reached { door: *door },
                // The replay told the host the notification the recorded run
                // died telling it, the tape's last record, every record before
                // it reproduced (`ReplayCore::emit`): the place the recorded
                // run died, as for a query asked again past the tape's end
                // below.
                (None, None) if st.died_in.is_some() => {
                    let door = st.died_in.clone();
                    match end {
                        Some(End::Unwound { message }) => Verdict::Panicked {
                            message: message.clone(),
                            door,
                        },
                        None => Verdict::Ended,
                        other => Verdict::Diverged(Divergence {
                            at,
                            door,
                            class: Class::Drift,
                            cause: Cause::OtherEnd {
                                recorded: recorded_end(other),
                                replayed: "stopped where the tape says the run died".to_string(),
                            },
                        }),
                    }
                }
                (None, Some(d))
                    if matches!(d.cause, Cause::Refused(ReplayDivergence::Exhausted { .. })) =>
                {
                    match (end, &st.past_end) {
                        // A tape with no end: everything on it reproduced.
                        (None, _) => Verdict::Ended,
                        // The recorded run died inside this door's live call,
                        // asking this request; the replay, every record before
                        // it reproduced, asked it again where the tape is over
                        // — the place the recorded run died, which a replay
                        // can only reach (it never makes the live call that
                        // panicked).
                        (Some(End::Unwound { message }), Some(asked))
                            if unwound_in.as_ref() == Some(asked) =>
                        {
                            Verdict::Panicked {
                                message: message.clone(),
                                door: Some(asked.0.clone()),
                            }
                        }
                        _ => Verdict::Diverged(d.clone()),
                    }
                }
                (None, Some(d)) => Verdict::Diverged(d.clone()),
                (None, None) => Verdict::Diverged(Divergence {
                    at,
                    door: None,
                    class: Class::Drift,
                    cause: Cause::Unreplayable {
                        why: "the replay stopped with no divergence said".to_string(),
                    },
                }),
            },
            Err(payload) => {
                let message = panic_message(payload.as_ref());
                match end {
                    Some(End::Unwound { message: recorded })
                        if *recorded == message && left == 0 =>
                    {
                        Verdict::Panicked {
                            message,
                            door: None,
                        }
                    }
                    other => Verdict::Diverged(Divergence {
                        at,
                        door: next_door,
                        class: Class::Drift,
                        cause: Cause::OtherEnd {
                            recorded: recorded_end(other),
                            replayed: format!("unwound: {message}"),
                        },
                    }),
                }
            }
        };
        Report {
            verdict,
            tape: header.tape.clone(),
            entry: header.entry.word().to_string(),
            recorded_by: header.build.clone(),
            replayed_by: aterm_types::version::build_id(),
            consumed: st.cursor.position(),
            effects: st.effects,
            requests: st.requests.clone(),
            told: st.told.clone(),
            lines: st.lines.clone(),
            returned,
            supplied: header.supplied.clone(),
            tail,
            mode: Mode::Strict,
            acts: None,
        }
    }

    /// The report of a COUNTERFACTUAL whose run ended `run` under
    /// `candidate` ([`super::acts`]), the rest as [`Self::report`]'s: the
    /// first divergent act (or the act the candidate went on without, or its
    /// end with acts left), a STALE read, the place the recorded run died in
    /// the last segment, a tape with no end spent (ENDED), else REPRODUCED —
    /// every act the tape holds taken as recorded, whatever was read, printed
    /// or decided on the way. Wherever the candidate did not take the act the
    /// recorded run took next, that act and the recorded decisions before it
    /// are said.
    fn report_acts(
        &self,
        header: &Header,
        run: std::thread::Result<String>,
        end: Option<&End>,
        unwound_in: Option<(String, Vec<u8>)>,
        tail: Option<String>,
        candidate: &Candidate,
    ) -> Report {
        let st = self.lock();
        let Cursor::Acts(c) = &st.cursor else {
            unreachable!("a counterfactual's report over a strict seam")
        };
        let records = c.records();
        let (next_act, _) = c.acts_left();
        let recorded_end = |end: Option<&End>| match end {
            Some(End::Returned { how }) => how.clone(),
            Some(End::Suspended) => "suspended".to_string(),
            Some(End::Unwound { message }) => format!("unwound: {message}"),
            None => "(no end on the tape)".to_string(),
        };
        let as_verdict = |d: &Divergence| match d.cause {
            Cause::Stale { .. } => Verdict::Stale(d.clone()),
            _ => Verdict::Diverged(d.clone()),
        };
        let mut returned = None;
        let verdict = match run {
            Ok(how) => {
                returned = Some(how);
                match next_act {
                    // The candidate ended with an act the recorded run took
                    // still ahead of it.
                    Some(n) => Verdict::Diverged(Divergence {
                        at: n as u64,
                        door: None,
                        class: Class::Decision,
                        cause: Cause::ActDiffers {
                            recorded: match &records[n] {
                                EffectRecord::Exchange { door, request, .. } => {
                                    Some(act_side(Some(n as u64), door, request))
                                }
                                _ => None,
                            },
                            asked: None,
                        },
                    }),
                    None => Verdict::Reproduced,
                }
            }
            Err(payload) if payload.is::<Halted>() => match (&st.reached, &st.diverged) {
                (Some(door), _) => Verdict::Reached { door: *door },
                (None, None) if st.died_in.is_some() => match end {
                    Some(End::Unwound { message }) => Verdict::Panicked {
                        message: message.clone(),
                        door: st.died_in.clone(),
                    },
                    None => Verdict::Ended,
                    other => Verdict::Diverged(Divergence {
                        at: c.position() as u64,
                        door: st.died_in.clone(),
                        class: Class::Drift,
                        cause: Cause::OtherEnd {
                            recorded: recorded_end(other),
                            replayed: "stopped where the tape says the run died".to_string(),
                        },
                    }),
                },
                // A tape with no end, refused where it is SPENT — no round
                // trip or file read left on it: everything on it was taken,
                // and what the recorded run asked next is not on it. Refused
                // with records left (an act, a read the candidate does not
                // ask as the recorded run did), it is what it is: a tape
                // with no end is a killed run or one still running, never a
                // licence to call the rest of it taken.
                (None, Some(_)) if end.is_none() && st.acts_spent => Verdict::Ended,
                // The recorded run died inside this door's live call asking
                // this request, asked again in the tape's last segment.
                (None, Some(d)) if st.past_end.is_some() => match end {
                    Some(End::Unwound { message }) if unwound_in == st.past_end => {
                        Verdict::Panicked {
                            message: message.clone(),
                            door: st.past_end.as_ref().map(|(door, _)| door.clone()),
                        }
                    }
                    _ => as_verdict(d),
                },
                (None, Some(d)) => as_verdict(d),
                (None, None) => Verdict::Diverged(Divergence {
                    at: c.position() as u64,
                    door: None,
                    class: Class::Drift,
                    cause: Cause::Unreplayable {
                        why: "the replay stopped with no divergence said".to_string(),
                    },
                }),
            },
            Err(payload) => {
                let message = panic_message(payload.as_ref());
                match end {
                    Some(End::Unwound { message: recorded }) if *recorded == message => {
                        Verdict::Panicked {
                            message,
                            door: None,
                        }
                    }
                    other => Verdict::Diverged(Divergence {
                        at: c.position() as u64,
                        door: None,
                        class: Class::Drift,
                        cause: Cause::OtherEnd {
                            recorded: recorded_end(other),
                            replayed: format!("unwound: {message}"),
                        },
                    }),
                }
            }
        };
        // The act the recorded run took where the candidate did not: a
        // divergent act's recorded side, the act it went on without, and —
        // where it stopped short otherwise (STALE, the progress guard, an
        // end of its own) — its segment's closing act.
        let named = |side: &ActSide| side.at.and_then(|n| usize::try_from(n).ok());
        let (untaken, said_by_cause) = match &verdict {
            Verdict::Diverged(Divergence {
                cause: Cause::ActDiffers { recorded, .. },
                ..
            }) => (recorded.as_ref().and_then(named), true),
            Verdict::Diverged(Divergence {
                cause: Cause::ActSkipped { recorded, .. },
                ..
            }) => (named(recorded), true),
            Verdict::Diverged(_) | Verdict::Stale(_) => (c.closing(), false),
            _ => (None, false),
        };
        let recorded_next = untaken
            .filter(|_| !said_by_cause)
            .and_then(|n| match records.get(n) {
                Some(EffectRecord::Exchange { door, request, .. }) => {
                    Some(act_side(Some(n as u64), door, request))
                }
                _ => None,
            });
        // The decisions the recorded run made in that segment before it
        // acted: its marks there, in order.
        let recorded_decisions = untaken.map_or_else(Vec::new, |n| {
            let n = n.min(records.len());
            records[c.segment_start().min(n)..n]
                .iter()
                .filter_map(|r| match r {
                    EffectRecord::Emit { door, bytes } if door.starts_with("decide.") => {
                        let mark = DecideMark::decode(std::str::from_utf8(bytes).ok()?)?;
                        Some(Decided {
                            door: door.clone(),
                            rule: mark.rule,
                            outcome: mark.outcome,
                        })
                    }
                    _ => None,
                })
                .collect()
        });
        // The candidate's, in the segment it stands in.
        let candidate_decisions = if st.decided_in == c.segment_start() {
            st.decided.clone()
        } else {
            Vec::new()
        };
        let last_screen = st
            .last_screen
            .and_then(|n| records.get(usize::try_from(n).ok()?))
            .and_then(|r| match r {
                EffectRecord::Exchange { door, reply, .. } if door == doors::CTL => {
                    <Result<CtlReply, String> as TapeValue>::from_tape(reply)
                        .ok()?
                        .ok()
                }
                _ => None,
            })
            .and_then(|reply| crate::supervise::screen::parse_text_json(&reply.stdout).ok())
            .map(|screen| screen.rows);
        Report {
            verdict,
            tape: header.tape.clone(),
            entry: header.entry.word().to_string(),
            recorded_by: header.build.clone(),
            replayed_by: aterm_types::version::build_id(),
            consumed: c.position(),
            effects: st.effects,
            requests: st.requests.clone(),
            told: st.told.clone(),
            lines: st.lines.clone(),
            returned,
            supplied: header.supplied.clone(),
            tail,
            mode: Mode::Acts,
            acts: Some(ActsReport {
                candidate: candidate.said(),
                account: c.account.clone(),
                last_screen,
                recorded_next,
                recorded_decisions,
                candidate_decisions,
            }),
        }
    }
}

#[cfg(test)]
#[path = "replay_tests.rs"]
pub(crate) mod tests;
