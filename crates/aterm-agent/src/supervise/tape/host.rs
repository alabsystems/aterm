// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE HOST'S DOOR ON A TAPE: what crosses when the loop asks the window's host ([`crate::supervise::IdleHost`]) — the two hosts the trait's macro implements it for (`supervise::run::host_door`), and the tape form of every argument and answer.

use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::codec::CodecError;
use super::core::TapeValue;
use super::frame::Hint;
use crate::harness::accounts::Sample;
use crate::harness::acts::{Act, LandHeld, TaskFace};
use crate::harness::disk::Level;
use crate::harness::repo_work::RepoWork;
use crate::harness::upgrade::{LastTurn, StandingLimit};
use crate::supervise::background::{BackgroundCall, BackgroundEvidence};
use crate::supervise::fx::Fx;
use crate::supervise::memory::{AgentMemory, Pressure};
use crate::supervise::occupancy::{OccupancyReading, OccupancySaid, PaceGrant, RuleId, SlotsSeen};
use crate::supervise::policy::exception::Exception;
use crate::supervise::policy::login::LoginShared;
use crate::supervise::policy::reset::{
    ResetAsk, ResetEnd, ResetGo, ResetNo, ResetOutcome, ResetWord,
};
use crate::supervise::policy::turn_end::{
    AccountAdvice, AccountBad, AccountPoint, HostTurn, Reach, Restart,
};
use crate::supervise::policy::turn_end::{HeldBy, Ready, RosterWord, SignInNext};
use crate::supervise::run::{
    Guard, HostStep, IdlePoint, ProbeMiss, ProbeNo, StallSeen, StuckSurvey, UsageSeen, WaitPlan,
};
use crate::supervise::tap_clock::{TapWindow, UsageWindow};

/// A RECORDING run's host ([module docs](self)): `inner`, the window's own (or a test's), asked through the run's door.
pub(crate) struct TapedHost<H: ?Sized> {
    inner: Arc<H>,
    fx: Fx,
}

impl<H: ?Sized> TapedHost<H> {
    /// `inner`, its calls taped through `fx` (a recording door).
    pub(crate) const fn new(inner: Arc<H>, fx: Fx) -> Self {
        Self { inner, fx }
    }

    /// The host every call is forwarded to.
    pub(crate) fn inner(&self) -> &H {
        &self.inner
    }

    /// A QUERY at `door`: `live` asks the host, and its answer — the live one — comes back, taped with `request`.
    pub(crate) fn ask<A: HostReply>(
        &self,
        door: &'static str,
        request: Vec<u8>,
        live: impl FnOnce() -> A,
        act: bool,
    ) -> A {
        let answer = self.fx.host_exchange(door, move || request, live);
        if act {
            self.fx.flush(Hint::Act);
        }
        answer
    }

    /// A NOTIFICATION at `door`, said with `request`, then told to the host by `live` (`Fx::host_tell`: a host that panics there leaves the door it died at on the tape).
    pub(crate) fn tell(
        &self,
        door: &'static str,
        request: Vec<u8>,
        live: impl FnOnce(),
        act: bool,
    ) {
        self.fx.host_tell(door, request, live);
        if act {
            self.fx.flush(Hint::Act);
        }
    }
}

impl<H: ?Sized + fmt::Debug> fmt::Debug for TapedHost<H> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TapedHost")
            .field("inner", &&*self.inner)
            .finish_non_exhaustive()
    }
}

/// A REPLAY's host ([module docs](self)): no host behind it, every answer the tape's.
#[derive(Debug)]
pub(crate) struct ReplayHost {
    fx: Fx,
}

impl ReplayHost {
    /// The host of a replay whose door is `fx`.
    /// # Panics
    /// `fx` does not replay: a host with nothing behind it over a door that would call its live world has nothing to call.
    pub(crate) fn new(fx: Fx) -> Self {
        assert!(
            fx.replaying(),
            "a replay's host over a door that does not replay"
        );
        Self { fx }
    }

    /// A QUERY at `door` asking `request`: the recorded answer.
    pub(crate) fn ask<A: HostReply>(&self, door: &'static str, request: Vec<u8>) -> A {
        self.fx.host_exchange(
            door,
            move || request,
            || unreachable!("a replaying door never makes its live call"),
        )
    }

    /// A NOTIFICATION at `door`: compared with the recorded one.
    pub(crate) fn tell(&self, door: &'static str, request: Vec<u8>) {
        self.fx.emit(door, move || request);
    }
}

/// An argument a host's call is taped with: its `Debug` text, compared as it is ([module docs](self)).
pub(crate) trait TapeArg: fmt::Debug {}

impl<T: TapeArg + ?Sized> TapeArg for &T {}
impl<T: TapeArg> TapeArg for Option<T> {}
impl<T: TapeArg, E: TapeArg> TapeArg for Result<T, E> {}
impl<A: TapeArg, B: TapeArg> TapeArg for (A, B) {}
/// A slice of arguments (`needs_standing`'s set): its `Debug` is each
/// element's in order, so a set told otherwise compares otherwise.
impl<T: TapeArg> TapeArg for [T] {}

/// Each type an argument of the host's may be, opted in by name: its `Debug` is the text a tape compares, and holds no `Instant`.
macro_rules! tape_arg_debug {
    ($($t:ty),* $(,)?) => {
        $(impl TapeArg for $t {})*
    };
}

tape_arg_debug!(
    str,
    String,
    bool,
    u32,
    u64,
    i64,
    IdlePoint,
    Restart,
    WaitPlan,
    Act,
    TaskFace,
    LandHeld,
    Guard,
    StuckSurvey,
    StallSeen,
    Exception,
    UsageSeen,
    AccountPoint,
    AccountBad,
    Sample,
    ProbeMiss,
    ResetAsk,
    ResetEnd,
    UsageWindow,
    // `occupancy_pace`'s rule: a closed enum, printed as its variant.
    RuleId,
    // `occupancy_said`'s word: closed words and plain numbers, no `Instant`.
    OccupancySaid,
);

/// A call's request as a tape holds it: `name=<Debug>` for each argument, a line each, in the signature's order; nothing for a call with none.
pub(crate) fn request(args: &[(&str, &dyn TapeArg)]) -> Vec<u8> {
    let text = args
        .iter()
        .map(|(name, arg)| format!("{name}={arg:?}"))
        .collect::<Vec<_>>()
        .join("\n");
    #[cfg(test)]
    assert!(
        !super::core::holds_instant(&text),
        "a host's argument holds a raw Instant: {text}"
    );
    text.into_bytes()
}

/// An answer of the host's, as a tape holds it: what [`TapedHost`] writes and [`ReplayHost`] reads back, with the tape's origin to place an `Instant` by ([module docs](self)).
pub(crate) trait HostReply: Sized {
    /// The answer's bytes on the tape, its instants from `origin`.
    fn put(&self, origin: Instant) -> Vec<u8>;

    /// The answer [`Self::put`] wrote, its instants placed from `origin`.
    /// # Errors
    /// Bytes [`Self::put`] did not write.
    fn take(bytes: &[u8], origin: Instant) -> Result<Self, CodecError>;

    /// The answer a replay gives at a door the recording build did not have ([`TapeValue::absent`]); `None`: such a door diverges, as any door the tape does not hold.
    fn absent() -> Option<Self> {
        None
    }
}

/// Every value a round trip already tapes holds no `Instant`.
impl<T: TapeValue> HostReply for T {
    fn put(&self, _: Instant) -> Vec<u8> {
        self.to_tape()
    }

    fn take(bytes: &[u8], _: Instant) -> Result<Self, CodecError> {
        T::from_tape(bytes)
    }

    fn absent() -> Option<Self> {
        T::absent()
    }
}

const NOT_WRITTEN: &str = "a host's answer this build did not write";

const fn bad(why: &'static str) -> CodecError {
    CodecError { at: 0, why }
}

/// `value` as JSON bytes, or no bytes where it has no JSON form. The mirrors
/// hold strings, numbers, booleans, and lists and options of them, which
/// always serialize, and a transcript's path ([`TranscriptWire`]), which
/// serializes only when it is UTF-8.
fn json<T: Serialize>(value: &T) -> Vec<u8> {
    aterm_json::to_vec(value).unwrap_or_default()
}

/// JSON bytes read back as `T`.
fn of_json<T: for<'de> Deserialize<'de>>(bytes: &[u8]) -> Result<T, CodecError> {
    aterm_json::from_slice(bytes).map_err(|_| bad(NOT_WRITTEN))
}

/// `at` as signed nanoseconds from `origin` (saturated past ±292 years).
pub(crate) fn offset_ns(origin: Instant, at: Instant) -> i64 {
    let after = at.saturating_duration_since(origin);
    let before = origin.saturating_duration_since(at);
    if before > after {
        i64::try_from(before.as_nanos()).map_or(i64::MIN, |n| -n)
    } else {
        i64::try_from(after.as_nanos()).unwrap_or(i64::MAX)
    }
}

/// The instant `ns` nanoseconds from `origin` ([`offset_ns`] read back).
/// # Errors
/// An offset this process's clock cannot hold.
pub(crate) fn instant_at(origin: Instant, ns: i64) -> Result<Instant, CodecError> {
    let span = Duration::from_nanos(ns.unsigned_abs());
    if ns < 0 {
        origin.checked_sub(span)
    } else {
        origin.checked_add(span)
    }
    .ok_or(bad("an instant this clock cannot hold"))
}

/// `wants_at`: the instant the host next wants a point, or none.
impl HostReply for Option<Instant> {
    fn put(&self, origin: Instant) -> Vec<u8> {
        json(&self.map(|at| offset_ns(origin, at)))
    }

    fn take(bytes: &[u8], origin: Instant) -> Result<Self, CodecError> {
        of_json::<Option<i64>>(bytes)?
            .map(|ns| instant_at(origin, ns))
            .transpose()
    }
}

/// [`Reach`] as a tape holds it.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum ReachWire {
    Unknown,
    Down { since: i64 },
    Up { since: i64 },
}

impl HostReply for Reach {
    fn put(&self, origin: Instant) -> Vec<u8> {
        json(&match *self {
            Reach::Unknown => ReachWire::Unknown,
            Reach::Down { since } => ReachWire::Down {
                since: offset_ns(origin, since),
            },
            Reach::Up { since } => ReachWire::Up {
                since: offset_ns(origin, since),
            },
        })
    }

    fn take(bytes: &[u8], origin: Instant) -> Result<Self, CodecError> {
        Ok(match of_json(bytes)? {
            ReachWire::Unknown => Reach::Unknown,
            ReachWire::Down { since } => Reach::Down {
                since: instant_at(origin, since)?,
            },
            ReachWire::Up { since } => Reach::Up {
                since: instant_at(origin, since)?,
            },
        })
    }
}

/// [`AccountAdvice`] as a tape holds it.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum AdviceWire {
    Probe { otherwise: Option<Box<AdviceWire>> },
    Move { to: String },
    Wait { until: i64 },
    Resume { cleared_at: i64 },
}

impl AdviceWire {
    fn of(advice: &AccountAdvice, origin: Instant) -> Self {
        match advice {
            AccountAdvice::Probe { otherwise } => AdviceWire::Probe {
                otherwise: otherwise
                    .as_deref()
                    .map(|o| Box::new(AdviceWire::of(o, origin))),
            },
            AccountAdvice::Move { to } => AdviceWire::Move { to: to.clone() },
            AccountAdvice::Wait { until } => AdviceWire::Wait {
                until: offset_ns(origin, *until),
            },
            AccountAdvice::Resume { cleared_at } => AdviceWire::Resume {
                cleared_at: *cleared_at,
            },
        }
    }

    fn advice(self, origin: Instant) -> Result<AccountAdvice, CodecError> {
        Ok(match self {
            AdviceWire::Probe { otherwise } => AccountAdvice::Probe {
                otherwise: otherwise
                    .map(|o| o.advice(origin).map(Box::new))
                    .transpose()?,
            },
            AdviceWire::Move { to } => AccountAdvice::Move { to },
            AdviceWire::Wait { until } => AccountAdvice::Wait {
                until: instant_at(origin, until)?,
            },
            AdviceWire::Resume { cleared_at } => AccountAdvice::Resume { cleared_at },
        })
    }
}

impl HostReply for Option<AccountAdvice> {
    fn put(&self, origin: Instant) -> Vec<u8> {
        json(&self.as_ref().map(|a| AdviceWire::of(a, origin)))
    }

    fn take(bytes: &[u8], origin: Instant) -> Result<Self, CodecError> {
        of_json::<Option<AdviceWire>>(bytes)?
            .map(|w| w.advice(origin))
            .transpose()
    }
}

/// The answers that are plain JSON values: a word, a flag, a count.
macro_rules! tape_json {
    ($($t:ty),* $(,)?) => {
        $(
            impl TapeValue for $t {
                fn to_tape(&self) -> Vec<u8> {
                    json(self)
                }

                fn from_tape(bytes: &[u8]) -> Result<Self, CodecError> {
                    of_json(bytes)
                }
            }
        )*
    };
}

tape_json!(Option<String>, Option<bool>, Option<u32>, Option<u64>, u64);

/// The session's login method ([`super::super::IdleHost::login_method`]) on a
/// tape: its word.
impl TapeValue for Option<aterm_phase::LoginMethod> {
    fn to_tape(&self) -> Vec<u8> {
        json(&self.map(aterm_phase::LoginMethod::word))
    }

    fn from_tape(bytes: &[u8]) -> Result<Self, CodecError> {
        of_json::<Option<String>>(bytes)?
            .map(|w| {
                aterm_phase::LoginMethod::of_word(&w)
                    .ok_or(bad("a login method this build does not know"))
            })
            .transpose()
    }
}

/// Another tab's sign-in of the session's login
/// ([`super::super::IdleHost::login_shared`]) as a tape holds it.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum SharedWire {
    Carried(String),
    Back(String),
}

impl TapeValue for Option<LoginShared> {
    fn to_tape(&self) -> Vec<u8> {
        json(&self.as_ref().map(|shared| match shared {
            LoginShared::Carried { by } => SharedWire::Carried(by.clone()),
            LoginShared::Back { by } => SharedWire::Back(by.clone()),
        }))
    }

    fn from_tape(bytes: &[u8]) -> Result<Self, CodecError> {
        Ok(
            of_json::<Option<SharedWire>>(bytes)?.map(|wire| match wire {
                SharedWire::Carried(by) => LoginShared::Carried { by },
                SharedWire::Back(by) => LoginShared::Back { by },
            }),
        )
    }
}

/// What the roster says of the session's login
/// ([`super::super::IdleHost::roster_word`]) as a tape holds it.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum RosterWire {
    Unlisted,
    /// The row to sign in next, and whether by renewal; no other row ready,
    /// and nothing of the roster's holding the tab ([`RosterWord::Row`]'s
    /// `held`) — what a tape an earlier build wrote says of any row with no
    /// other ready, read so.
    Row(Option<(String, bool)>),
    /// The same, with the other row ready to run now
    /// ([`RosterWord::Row`]'s `ready`).
    Ready {
        next: Option<(String, bool)>,
        ready: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        held: Option<String>,
    },
    /// No other row ready, and what holds the tab at the limit even once one
    /// is ([`held_word`]).
    Held {
        next: Option<(String, bool)>,
        held: String,
    },
    Unread,
}

/// [`HeldBy`]'s word on a tape.
const fn held_word(held: HeldBy) -> &'static str {
    match held {
        HeldBy::Earlier => "earlier",
        HeldBy::Pin => "pin",
        HeldBy::Reset => "reset",
    }
}

fn held_of(word: &str) -> Result<HeldBy, CodecError> {
    [HeldBy::Earlier, HeldBy::Pin, HeldBy::Reset]
        .into_iter()
        .find(|held| held_word(*held) == word)
        .ok_or(bad("a hold of the roster's this build does not know"))
}

impl TapeValue for Option<RosterWord> {
    fn to_tape(&self) -> Vec<u8> {
        json(&self.as_ref().map(|word| match word {
            RosterWord::Unlisted => RosterWire::Unlisted,
            RosterWord::Row { next, ready, held } => {
                let next = next.as_ref().map(|n| (n.label.clone(), n.renew));
                let held = held.map(|held| held_word(held).to_string());
                match (ready, held) {
                    (Some(ready), held) => RosterWire::Ready {
                        next,
                        ready: ready.label.clone(),
                        held,
                    },
                    (None, Some(held)) => RosterWire::Held { next, held },
                    (None, None) => RosterWire::Row(next),
                }
            }
            RosterWord::Unread => RosterWire::Unread,
        }))
    }

    fn from_tape(bytes: &[u8]) -> Result<Self, CodecError> {
        let next_of =
            |next: Option<(String, bool)>| next.map(|(label, renew)| SignInNext { label, renew });
        of_json::<Option<RosterWire>>(bytes)?
            .map(|wire| {
                Ok(match wire {
                    RosterWire::Unlisted => RosterWord::Unlisted,
                    RosterWire::Row(next) => RosterWord::Row {
                        next: next_of(next),
                        ready: None,
                        held: None,
                    },
                    RosterWire::Ready { next, ready, held } => RosterWord::Row {
                        next: next_of(next),
                        ready: Some(Ready { label: ready }),
                        held: held.as_deref().map(held_of).transpose()?,
                    },
                    RosterWire::Held { next, held } => RosterWord::Row {
                        next: next_of(next),
                        ready: None,
                        held: Some(held_of(&held)?),
                    },
                    RosterWire::Unread => RosterWord::Unread,
                })
            })
            .transpose()
    }
}

/// [`HostTurn`]'s word on a tape.
const fn turn_word(turn: HostTurn) -> &'static str {
    match turn {
        HostTurn::Notice => "notice",
        HostTurn::CarryOn => "carry-on",
        HostTurn::Check => "check",
        HostTurn::Fact => "fact",
    }
}

fn turn_of(word: &str) -> Result<HostTurn, CodecError> {
    [
        HostTurn::Notice,
        HostTurn::CarryOn,
        HostTurn::Check,
        HostTurn::Fact,
    ]
    .into_iter()
    .find(|t| turn_word(*t) == word)
    .ok_or(bad("a host turn this build does not know"))
}

/// [`HostStep`] as a tape holds it.
#[derive(Serialize, Deserialize)]
struct StepWire {
    line: String,
    moved: bool,
    typed: Option<String>,
    repeat: bool,
}

impl TapeValue for Option<HostStep> {
    fn to_tape(&self) -> Vec<u8> {
        json(&self.as_ref().map(|s| {
            let HostStep {
                line,
                moved,
                typed,
                repeat,
            } = s;
            StepWire {
                line: line.clone(),
                moved: *moved,
                typed: typed.map(|t| turn_word(t).to_string()),
                repeat: *repeat,
            }
        }))
    }

    fn from_tape(bytes: &[u8]) -> Result<Self, CodecError> {
        of_json::<Option<StepWire>>(bytes)?
            .map(|w| {
                Ok(HostStep {
                    line: w.line,
                    moved: w.moved,
                    typed: w.typed.as_deref().map(turn_of).transpose()?,
                    repeat: w.repeat,
                })
            })
            .transpose()
    }
}

/// `at_background`'s answer as a tape holds it: the line the host typed at
/// a break and the turn it is ([`HostTurn`]).
#[derive(Serialize, Deserialize)]
struct BackgroundWire {
    line: String,
    turn: String,
}

impl TapeValue for Option<(String, HostTurn)> {
    fn to_tape(&self) -> Vec<u8> {
        json(&self.as_ref().map(|(line, turn)| BackgroundWire {
            line: line.clone(),
            turn: turn_word(*turn).to_string(),
        }))
    }

    /// A tape an earlier build wrote holds the line alone, from before a line
    /// typed at a break had a kind: it reads back as the notice, as that
    /// build's loop took every such line.
    fn from_tape(bytes: &[u8]) -> Result<Self, CodecError> {
        match of_json::<Option<BackgroundWire>>(bytes) {
            Ok(wire) => wire.map(|w| Ok((w.line, turn_of(&w.turn)?))).transpose(),
            Err(_) => Ok(of_json::<Option<String>>(bytes)?.map(|line| (line, HostTurn::Notice))),
        }
    }
}

/// `background_jobs`'s answer as a tape holds it
/// ([`BackgroundEvidence`]): `"unknown"`, or the call that made no progress.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum EvidenceWire {
    Unknown,
    NoProgress {
        pid: u32,
        pgid: u32,
        exe: String,
        task: Option<String>,
        quiet_ms: u64,
        wait: Option<String>,
    },
}

impl TapeValue for BackgroundEvidence {
    fn to_tape(&self) -> Vec<u8> {
        json(&match self {
            BackgroundEvidence::Unknown => EvidenceWire::Unknown,
            BackgroundEvidence::NoProgress { worst } => {
                let BackgroundCall {
                    pid,
                    pgid,
                    exe,
                    task,
                    quiet_ms,
                    wait,
                } = worst;
                EvidenceWire::NoProgress {
                    pid: *pid,
                    pgid: *pgid,
                    exe: exe.clone(),
                    task: task.clone(),
                    quiet_ms: *quiet_ms,
                    wait: wait.clone(),
                }
            }
        })
    }

    fn from_tape(bytes: &[u8]) -> Result<Self, CodecError> {
        Ok(match of_json(bytes)? {
            EvidenceWire::Unknown => BackgroundEvidence::Unknown,
            EvidenceWire::NoProgress {
                pid,
                pgid,
                exe,
                task,
                quiet_ms,
                wait,
            } => BackgroundEvidence::NoProgress {
                worst: BackgroundCall {
                    pid,
                    pgid,
                    exe,
                    task,
                    quiet_ms,
                    wait,
                },
            },
        })
    }
}

/// `occupancy`'s answer as a tape holds it ([`OccupancyReading`]): numbers and
/// closed words (the memory pressure's and the disk level's), never a path or
/// a name.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
struct OccupancyWire {
    taken_ms: i64,
    stale: bool,
    avail_mib: Option<u64>,
    pressure: Option<String>,
    disk: Option<String>,
    slots: Option<SlotsWire>,
    slots_free_ms: Option<u64>,
    room: Option<bool>,
    held_slot_last_ms: Option<i64>,
}

/// [`SlotsSeen`] as a tape holds it.
#[derive(Serialize, Deserialize)]
struct SlotsWire {
    usable: u16,
    n: u16,
}

/// The disk watch's [`Level`] on a tape, by its own word.
fn level_of(word: &str) -> Result<Level, CodecError> {
    [
        Level::Unknown,
        Level::Ok,
        Level::Low,
        Level::Short,
        Level::Emergency,
    ]
    .into_iter()
    .find(|l| l.as_str() == word)
    .ok_or(bad("a disk level this build does not know"))
}

/// A DOOR NEWER THAN THE TAPE: a recording build with no `host.occupancy`
/// asked nothing there, and its loop judged no rule on the machine, so its
/// replay answers the empty reading ([`TapeValue::absent`]) — where the
/// tape's header says that build lacked the door, and only there.
impl TapeValue for OccupancyReading {
    fn to_tape(&self) -> Vec<u8> {
        let OccupancyReading {
            taken_ms,
            stale,
            avail_mib,
            pressure,
            disk,
            slots,
            slots_free_ms,
            room,
            held_slot_last_ms,
        } = *self;
        json(&OccupancyWire {
            taken_ms,
            stale,
            avail_mib,
            pressure: pressure.map(|p| pressure_word(p).to_string()),
            disk: disk.map(|l| l.as_str().to_string()),
            slots: slots.map(|SlotsSeen { usable, n }| SlotsWire { usable, n }),
            slots_free_ms,
            room,
            held_slot_last_ms,
        })
    }

    fn from_tape(bytes: &[u8]) -> Result<Self, CodecError> {
        let w = of_json::<OccupancyWire>(bytes)?;
        Ok(OccupancyReading {
            taken_ms: w.taken_ms,
            stale: w.stale,
            avail_mib: w.avail_mib,
            pressure: w.pressure.as_deref().map(pressure_of).transpose()?,
            disk: w.disk.as_deref().map(level_of).transpose()?,
            slots: w.slots.map(|s| SlotsSeen {
                usable: s.usable,
                n: s.n,
            }),
            slots_free_ms: w.slots_free_ms,
            room: w.room,
            held_slot_last_ms: w.held_slot_last_ms,
        })
    }

    fn absent() -> Option<Self> {
        Some(OccupancyReading::default())
    }
}

/// `occupancy_pace`'s answer on a tape: its word ([`PaceGrant::word`]), as a
/// JSON string. A word this build does not know is refused. No absent form:
/// a build without the door typed no fact line, so a replay that reaches the
/// door diverges there.
impl TapeValue for PaceGrant {
    fn to_tape(&self) -> Vec<u8> {
        json(&self.word())
    }

    fn from_tape(bytes: &[u8]) -> Result<Self, CodecError> {
        PaceGrant::of_word(&of_json::<String>(bytes)?)
            .ok_or(bad("a pace answer this build does not know"))
    }
}

/// [`StandingLimit`] as a tape holds it.
#[derive(Serialize, Deserialize)]
struct LimitWire {
    text: String,
    until: Option<u64>,
    window: Option<String>,
    at: Option<u64>,
    row: Option<String>,
}

impl TapeValue for Option<StandingLimit> {
    fn to_tape(&self) -> Vec<u8> {
        json(&self.as_ref().map(|l| {
            let StandingLimit {
                text,
                until,
                window,
                at,
                row,
            } = l;
            LimitWire {
                text: text.clone(),
                until: *until,
                window: window.clone(),
                at: *at,
                row: row.clone(),
            }
        }))
    }

    fn from_tape(bytes: &[u8]) -> Result<Self, CodecError> {
        Ok(of_json::<Option<LimitWire>>(bytes)?.map(|w| StandingLimit {
            text: w.text,
            until: w.until,
            window: w.window,
            at: w.at,
            row: w.row,
        }))
    }
}

/// The usage tap's newest reading of one window
/// ([`super::super::IdleHost::tap_window`]) as a tape holds it: the window's
/// figure (its whole percent, its reset in unix seconds and its tenths, as
/// [`crate::harness::tap::Window`] holds them), and when the tap wrote it.
#[derive(Serialize, Deserialize)]
struct TapWindowWire {
    figure: Option<(u32, i64, Option<u32>)>,
    read_at: i64,
}

impl TapeValue for Option<TapWindow> {
    fn to_tape(&self) -> Vec<u8> {
        json(&self.map(|w| TapWindowWire {
            figure: w.figure.map(|f| (f.pct, f.resets_at, f.pct_x10)),
            read_at: w.read_at,
        }))
    }

    fn from_tape(bytes: &[u8]) -> Result<Self, CodecError> {
        Ok(of_json::<Option<TapWindowWire>>(bytes)?.map(|w| TapWindow {
            figure: w
                .figure
                .map(|(pct, resets_at, pct_x10)| crate::harness::tap::Window {
                    pct,
                    resets_at,
                    pct_x10,
                }),
            read_at: w.read_at,
        }))
    }
}

/// [`LastTurn`] as a tape holds it, the time its prompt's turn worked in
/// nanoseconds.
#[derive(Serialize, Deserialize)]
struct LastTurnWire {
    last: Option<String>,
    opened: Option<String>,
    opened_said: Option<u64>,
    ended_ns: Option<u64>,
    /// Absent on a tape recorded before it: no time.
    #[serde(default)]
    opened_ms: Option<u64>,
    asked: Vec<String>,
}

impl TapeValue for Option<LastTurn> {
    fn to_tape(&self) -> Vec<u8> {
        json(&self.as_ref().map(|t| {
            let LastTurn {
                last,
                opened,
                opened_said,
                ended,
                opened_ms,
                asked,
            } = t;
            LastTurnWire {
                last: last.clone(),
                opened: opened.clone(),
                opened_said: *opened_said,
                ended_ns: ended.map(|d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX)),
                opened_ms: *opened_ms,
                asked: asked.clone(),
            }
        }))
    }

    fn from_tape(bytes: &[u8]) -> Result<Self, CodecError> {
        Ok(of_json::<Option<LastTurnWire>>(bytes)?.map(|w| LastTurn {
            last: w.last,
            opened: w.opened,
            opened_said: w.opened_said,
            ended: w.ended_ns.map(Duration::from_nanos),
            opened_ms: w.opened_ms,
            asked: w.asked,
        }))
    }
}

/// [`RepoWork`] as a tape holds it. A tape recorded before the probe read
/// the remote's default branch holds no `unlanded` or `main_ref`: none known.
#[derive(Serialize, Deserialize)]
struct RepoWorkWire {
    repo: String,
    changed: usize,
    untracked: usize,
    stashes: usize,
    unpushed: usize,
    #[serde(default)]
    unlanded: usize,
    #[serde(default)]
    main_ref: String,
    paths: Vec<String>,
    /// The work's state ([`RepoWork::state`]); a tape recorded before the
    /// probe read it holds none: not read.
    #[serde(default)]
    state: Option<u64>,
}

impl TapeValue for Option<RepoWork> {
    fn to_tape(&self) -> Vec<u8> {
        json(&self.as_ref().map(|w| {
            let RepoWork {
                repo,
                changed,
                untracked,
                stashes,
                unpushed,
                unlanded,
                main_ref,
                paths,
                state,
            } = w;
            RepoWorkWire {
                repo: repo.clone(),
                changed: *changed,
                untracked: *untracked,
                stashes: *stashes,
                unpushed: *unpushed,
                unlanded: *unlanded,
                main_ref: main_ref.clone(),
                paths: paths.clone(),
                state: *state,
            }
        }))
    }

    fn from_tape(bytes: &[u8]) -> Result<Self, CodecError> {
        Ok(of_json::<Option<RepoWorkWire>>(bytes)?.map(|w| RepoWork {
            repo: w.repo,
            changed: w.changed,
            untracked: w.untracked,
            stashes: w.stashes,
            unpushed: w.unpushed,
            unlanded: w.unlanded,
            main_ref: w.main_ref,
            paths: w.paths,
            state: w.state,
        }))
    }
}

/// [`Pressure`]'s word on a tape.
const fn pressure_word(p: Pressure) -> &'static str {
    match p {
        Pressure::Normal => "normal",
        Pressure::Warn => "warn",
        Pressure::Critical => "critical",
    }
}

fn pressure_of(word: &str) -> Result<Pressure, CodecError> {
    [Pressure::Normal, Pressure::Warn, Pressure::Critical]
        .into_iter()
        .find(|p| pressure_word(*p) == word)
        .ok_or(bad("a memory pressure this build does not know"))
}

/// [`AgentMemory`] as a tape holds it.
#[derive(Serialize, Deserialize)]
struct MemoryWire {
    footprint_mib: u64,
    physical_mib: Option<u64>,
    available_mib: Option<u64>,
    used_mib: Option<u64>,
    pressure: Option<String>,
    swap_kib_s: Option<u32>,
    largest_other_mib: Option<u64>,
}

impl TapeValue for Option<AgentMemory> {
    fn to_tape(&self) -> Vec<u8> {
        json(&self.as_ref().map(|m| {
            let AgentMemory {
                footprint_mib,
                physical_mib,
                available_mib,
                used_mib,
                pressure,
                swap_kib_s,
                largest_other_mib,
            } = *m;
            MemoryWire {
                footprint_mib,
                physical_mib,
                available_mib,
                used_mib,
                pressure: pressure.map(|p| pressure_word(p).to_string()),
                swap_kib_s,
                largest_other_mib,
            }
        }))
    }

    fn from_tape(bytes: &[u8]) -> Result<Self, CodecError> {
        of_json::<Option<MemoryWire>>(bytes)?
            .map(|w| {
                Ok(AgentMemory {
                    footprint_mib: w.footprint_mib,
                    physical_mib: w.physical_mib,
                    available_mib: w.available_mib,
                    used_mib: w.used_mib,
                    pressure: w.pressure.as_deref().map(pressure_of).transpose()?,
                    swap_kib_s: w.swap_kib_s,
                    largest_other_mib: w.largest_other_mib,
                })
            })
            .transpose()
    }
}

/// `transcript`'s answer as a tape holds it: the file and its tail. A path
/// that is not UTF-8 has no JSON form, so its answer is one a replay cannot
/// take, and says so.
#[derive(Serialize, Deserialize)]
struct TranscriptWire {
    path: PathBuf,
    tail: String,
}

impl TapeValue for Option<(PathBuf, String)> {
    fn to_tape(&self) -> Vec<u8> {
        json(&self.as_ref().map(|(path, tail)| TranscriptWire {
            path: path.clone(),
            tail: tail.clone(),
        }))
    }

    fn from_tape(bytes: &[u8]) -> Result<Self, CodecError> {
        Ok(of_json::<Option<TranscriptWire>>(bytes)?.map(|w| (w.path, w.tail)))
    }
}

/// `account_probe`'s answer: `null` for a claim, else the refusal's word.
impl TapeValue for Result<(), ProbeNo> {
    fn to_tape(&self) -> Vec<u8> {
        json(&self.err().map(ProbeNo::word))
    }

    fn from_tape(bytes: &[u8]) -> Result<Self, CodecError> {
        match of_json::<Option<String>>(bytes)? {
            None => Ok(Ok(())),
            Some(word) => ProbeNo::of_word(&word)
                .map(Err)
                .ok_or(bad("a probe's refusal this build does not give")),
        }
    }
}

/// [`ResetWord`] as a tape holds it.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum ResetWordWire {
    Take {
        until: i64,
        long: bool,
        limits: Vec<String>,
        /// The row's kind by its vendor id (`aterm_phase::ResetKind::word`).
        kind: String,
        left: u32,
        day: String,
    },
    Again,
    No(String),
}

impl TapeValue for ResetWord {
    fn to_tape(&self) -> Vec<u8> {
        json(&match self {
            ResetWord::Take(go) => {
                let ResetGo {
                    until,
                    long,
                    limits,
                    kind,
                    left,
                    day,
                } = go;
                ResetWordWire::Take {
                    until: *until,
                    long: *long,
                    limits: limits.clone(),
                    kind: kind.word().to_string(),
                    left: *left,
                    day: day.clone(),
                }
            }
            ResetWord::Again => ResetWordWire::Again,
            ResetWord::No(no) => ResetWordWire::No(no.word().to_string()),
        })
    }

    fn from_tape(bytes: &[u8]) -> Result<Self, CodecError> {
        Ok(match of_json(bytes)? {
            ResetWordWire::Take {
                until,
                long,
                limits,
                kind,
                left,
                day,
            } => ResetWord::Take(ResetGo {
                until,
                long,
                limits,
                kind: kind_of(&kind)?,
                left,
                day,
            }),
            ResetWordWire::Again => ResetWord::Again,
            ResetWordWire::No(word) => ResetWord::No(
                ResetNo::of_word(&word).ok_or(bad("a reset's refusal this build does not give"))?,
            ),
        })
    }
}

/// A kind of reset by its vendor id, as a tape holds it: one this build
/// knows, or the value is refused.
fn kind_of(word: &str) -> Result<aterm_phase::ResetKind, CodecError> {
    aterm_phase::ResetKind::of_word(word).ok_or(bad("a kind of reset this build does not know"))
}

/// [`ResetOutcome`] as a tape holds it; a spend with no kind, as an earlier
/// build taped it, is refused ([`ResetWordWire`] says why).
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum OutcomeWire {
    Pending,
    Spent {
        /// The kind spent, by its vendor id (`aterm_phase::ResetKind::word`).
        kind: String,
        asks: bool,
        left: Option<u32>,
        cleared_at: i64,
    },
    /// [`ResetOutcome::Walled`]; `kind` as for `Spent`.
    Walled {
        kind: String,
        left: u32,
    },
    Lifted {
        asks: bool,
        cleared_at: i64,
    },
    Kept,
    Failed {
        login: bool,
    },
    Unread,
}

impl TapeValue for ResetOutcome {
    fn to_tape(&self) -> Vec<u8> {
        json(&match *self {
            ResetOutcome::Pending => OutcomeWire::Pending,
            ResetOutcome::Spent {
                kind,
                asks,
                left,
                cleared_at,
            } => OutcomeWire::Spent {
                kind: kind.word().to_string(),
                asks,
                left,
                cleared_at,
            },
            ResetOutcome::Walled { kind, left } => OutcomeWire::Walled {
                kind: kind.word().to_string(),
                left,
            },
            ResetOutcome::Lifted { asks, cleared_at } => OutcomeWire::Lifted { asks, cleared_at },
            ResetOutcome::Kept => OutcomeWire::Kept,
            ResetOutcome::Failed { login } => OutcomeWire::Failed { login },
            ResetOutcome::Unread => OutcomeWire::Unread,
        })
    }

    fn from_tape(bytes: &[u8]) -> Result<Self, CodecError> {
        Ok(match of_json(bytes)? {
            OutcomeWire::Pending => ResetOutcome::Pending,
            OutcomeWire::Spent {
                kind,
                asks,
                left,
                cleared_at,
            } => ResetOutcome::Spent {
                kind: kind_of(&kind)?,
                asks,
                left,
                cleared_at,
            },
            OutcomeWire::Walled { kind, left } => ResetOutcome::Walled {
                kind: kind_of(&kind)?,
                left,
            },
            OutcomeWire::Lifted { asks, cleared_at } => ResetOutcome::Lifted { asks, cleared_at },
            OutcomeWire::Kept => ResetOutcome::Kept,
            OutcomeWire::Failed { login } => ResetOutcome::Failed { login },
            OutcomeWire::Unread => ResetOutcome::Unread,
        })
    }
}

#[cfg(test)]
#[path = "host_tests.rs"]
mod tests;
