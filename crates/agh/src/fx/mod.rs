// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE effect seam (design §21.2).

pub mod canon;
pub mod child;
#[cfg(any(test, debug_assertions))]
mod sim;
pub mod tape;

use std::io::Read;
use std::path::Path;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use astream_effects::{
    EffectRecord, EffectSeam, EffectsLog, LiveSeam, OsEffects, RecordingSeam, ReplayDivergence,
    ReplaySeam,
};

use crate::error::{Error, Replay, Result, Sub};
use crate::policy::{Decision, RouteRequest};
use crate::redact::Redactor;
use crate::wire::{Endpoints, Transport, WireError, WireRequest, WireResponse};

/// A GitHub round trip's door: an exchange.
pub const DOOR_GH: &str = "gh";
/// A sleep's door: an exchange, the length in decimal nanoseconds out and nothing back.
pub const DOOR_SLEEP: &str = "sleep";
/// stdout's door: an emit of the bytes as rendered.
pub const DOOR_STDOUT: &str = "stdout";
/// The exit's door: an emit of the exit code in decimal.
pub const DOOR_EXIT: &str = "exit";
/// A routing decision's door (design §21.3): an exchange, the decision's inputs out and the
/// decision back, so a credentialed tape replays with no agh.toml and no token.
pub const DOOR_ROUTE: &str = "route";
/// The write journal's door: an emit of each record's body (design §6.8).
pub const DOOR_JOURNAL: &str = "journal";
/// The fabric's door: an emit of each post's request line (design §21.5).
pub const DOOR_FABRIC: &str = "fabric";
/// The git runner's door: an exchange, the run's canonical command out and its exit,
/// stdout and stderr back.
pub const DOOR_GIT: &str = "git";
/// A child's door (`gh auth token`): never taped. `auth import` is a Provision verb, and
/// tapes refuse those.
pub const DOOR_CHILD: &str = "child";

/// Which world the doors reach.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// The shipped seam: the OS clocks, real sleeps, the transport called directly.
    Live,
    /// Tests: a virtual clock only sleeps move; the transport called directly.
    #[cfg(any(test, debug_assertions))]
    Simulated,
    /// `--record`: live, every door taped into a new file.
    Recording,
    /// `--replay`: every door answered from a tape; nothing sent, no time passes.
    Replaying,
    /// `--replay OLD --record NEW`.
    Rerecording,
}

enum Seam {
    /// The OS clocks, behind a mutex held for one clock read.
    Live(Mutex<LiveSeam<OsEffects>>),
    /// A test's virtual clocks.
    #[cfg(any(test, debug_assertions))]
    Simulated(sim::SimClock),
    /// The OS.
    Recording {
        /// The recording seam, which owns one clone of the sink.
        seam: Mutex<RecordingSeam<OsEffects, tape::TapeSink>>,
        /// The other clone: `finish` flushes the file through it.
        sink: tape::TapeSink,
    },
    /// A tape's records, answered in order.
    Replaying {
        /// The replay seam over the tape's records.
        seam: Mutex<ReplaySeam>,
        /// How many records the tape holds.
        len: usize,
        /// The `next` of a divergence: `record it live: agh --record NEW {argv}` (`live_next`).
        live: String,
    },
    /// A tape's exchanges and clocks.
    Rerecording {
        /// The replay seam over the old tape's records, less its emits.
        from: Mutex<ReplaySeam>,
        /// How many records `from` holds.
        len: usize,
        /// The new tape.
        to: tape::TapeSink,
        /// The `next` of a divergence.
        live: String,
    },
}

/// The effect seam.
pub struct Fx {
    seam: Seam,
    /// The first divergence or refusal under a tape: every later door answers it (plan §4.6.27).
    latch: Mutex<Option<Error>>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn infallible<T>(read: std::result::Result<T, std::convert::Infallible>) -> T {
    match read {
        Ok(value) => value,
        Err(never) => match never {},
    }
}

fn read_whole(
    transport: &dyn Transport,
    mut req: WireRequest<'_>,
    cut: impl FnOnce(Vec<(String, String)>) -> Vec<(String, String)>,
) -> std::result::Result<canon::Reply, WireError> {
    let limit = req.limit;
    req.limit = u64::MAX;
    let mut response = transport.round_trip(req)?;
    let body = crate::wire::read_capped(&mut response.body, limit)?;
    Ok(canon::Reply {
        status: response.status,
        headers: cut(response.headers),
        body,
    })
}

fn kept_response(endpoints: &Endpoints, reply: canon::Reply) -> WireResponse {
    canon::Reply {
        headers: canon::keep(endpoints, reply.headers),
        ..reply
    }
    .into_response()
}

fn sleep_request(d: Duration) -> Vec<u8> {
    d.as_nanos().to_string().into_bytes()
}

fn diverged(d: &ReplayDivergence) -> Error {
    Error::replay(Replay::of(d), Redactor::shapes(&d.to_string()))
}

fn stream_not_taped() -> Error {
    Error::replay(
        Replay::StreamNotTaped,
        "a streamed answer is not taped: M1's tapes hold whole answers alone",
    )
    .with_next("run it without --record or --replay: a tape never holds a streamed answer")
}

fn child_not_taped() -> Error {
    Error::replay(
        Replay::StreamNotTaped,
        "a child process is not taped: the verbs that run one provision, and tapes refuse those",
    )
    .with_next("run it without --record or --replay: a tape never runs a child")
}

fn ended(d: &ReplayDivergence, len: usize) -> Error {
    match d {
        ReplayDivergence::Unreached { position, .. } => Error::replay(
            Replay::of(d),
            format!(
                "the tape holds {} more records after the run's exit",
                len.saturating_sub(*position)
            ),
        ),
        _ => diverged(d),
    }
}

fn shaped_request() -> Error {
    Error::replay(
        Replay::StreamNotTaped,
        "this request holds a token shape, which a tape redacts: a tape of it could not replay",
    )
    .with_next("run it without --record: a tape cannot hold this request")
}

fn shaped_reply() -> Error {
    Error::replay(
        Replay::BodyNotTaped,
        "GitHub's answer holds a token shape, which a tape redacts: a tape of it could not \
         replay",
    )
    .with_next("run it without --record: a tape cannot hold this answer")
}

fn shell_word(arg: &str) -> String {
    let plain = |b: u8| b.is_ascii_alphanumeric() || b"_./:=@%+,-".contains(&b);
    if !arg.is_empty() && arg.bytes().all(plain) {
        return arg.to_string();
    }
    format!("'{}'", arg.replace('\'', r"'\''"))
}

fn live_next(header: &tape::TapeHeader) -> String {
    let mut live = String::from("record it live: agh --record NEW");
    for arg in &header.argv {
        live.push(' ');
        live.push_str(&shell_word(&Redactor::shapes(arg)));
    }
    live
}

fn not_read(path: &Path, e: &std::io::Error) -> Error {
    Error::could_not_run(format!(
        "the tape {} could not be read: {e}",
        path.display()
    ))
    .with_sub(Sub::Local)
}

/// The tape at `path`, read whole (at most `tape::TAPE_MAX` bytes) and parsed.
/// # Errors
/// CouldNotRun, `Sub::Local`, why `the tape {path} could not be read: {e}`.
pub(crate) fn read_tape(path: &Path) -> Result<(tape::TapeHeader, Vec<EffectRecord>)> {
    let file = std::fs::File::open(path).map_err(|e| not_read(path, &e))?;
    let mut bytes = Vec::new();
    file.take(tape::TAPE_MAX.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|e| not_read(path, &e))?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > tape::TAPE_MAX {
        return Err(Error::replay(
            Replay::Format,
            format!(
                "the tape {} is over {} bytes, the most a replay reads",
                path.display(),
                tape::TAPE_MAX
            ),
        )
        .with_next(tape::RECORD_AGAIN));
    }
    let text = String::from_utf8(bytes).map_err(|_| {
        Error::replay(
            Replay::Format,
            format!("the tape {} is not UTF-8 text", path.display()),
        )
        .with_next(tape::RECORD_AGAIN)
    })?;
    tape::parse(&text)
}

impl Fx {
    const fn over(seam: Seam) -> Fx {
        Fx {
            seam,
            latch: Mutex::new(None),
        }
    }

    /// The shipped seam.
    #[must_use]
    pub fn live() -> Fx {
        Fx::over(Seam::Live(Mutex::new(LiveSeam::new(OsEffects::new()))))
    }

    /// Tests: `mono` starts at 0 and `wall_secs` at `start_unix_secs`.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub const fn simulated(start_unix_secs: u64) -> Fx {
        Fx::over(Seam::Simulated(sim::SimClock::new(start_unix_secs)))
    }

    /// Tape this run into `path`, which must not exist.
    /// # Errors
    /// CouldNotRun, `Sub::Local`.
    pub(crate) fn record(path: &Path, header: &tape::TapeHeader) -> Result<Fx> {
        let sink = tape::TapeSink::create(path, header)?;
        Ok(Fx::over(Seam::Recording {
            seam: Mutex::new(RecordingSeam::with_sink(OsEffects::new(), sink.clone())),
            sink,
        }))
    }

    /// Replay the tape at `path`.
    /// # Errors
    /// CouldNotRun, `Sub::Local`, why `the tape {path} could not be read: {e}`.
    #[cfg(any(test, debug_assertions))]
    pub(crate) fn replay(path: &Path) -> Result<(Fx, tape::TapeHeader)> {
        let (header, records) = read_tape(path)?;
        Ok((Fx::replay_from(&header, records), header))
    }

    /// `replay` over a tape's header and records already read (`read_tape`).
    pub(crate) fn replay_from(header: &tape::TapeHeader, records: Vec<EffectRecord>) -> Fx {
        let len = records.len();
        Fx::over(Seam::Replaying {
            seam: Mutex::new(ReplaySeam::new(EffectsLog(records))),
            len,
            live: live_next(header),
        })
    }

    /// Answer every exchange, clock and sleep from `old`, and tape them again, with this run's
    /// stdout and exit, into `new`.
    /// # Errors
    /// `replay`'s for `old`, `record`'s for `new`.
    #[cfg(any(test, debug_assertions))]
    pub(crate) fn rerecord(old: &Path, new: &Path) -> Result<(Fx, tape::TapeHeader)> {
        let (header, records) = read_tape(old)?;
        let fx = Fx::rerecord_from(&header, records, new)?;
        Ok((fx, header))
    }

    /// `rerecord` over `old`'s header and records already read (`read_tape`).
    /// # Errors
    /// `record`'s for `new`.
    pub(crate) fn rerecord_from(
        header: &tape::TapeHeader,
        mut records: Vec<EffectRecord>,
        new: &Path,
    ) -> Result<Fx> {
        records.retain(|record| !matches!(record, EffectRecord::Emit { .. }));
        let renewed = tape::TapeHeader {
            agh_version: crate::VERSION.to_string(),
            ..header.clone()
        };
        let to = tape::TapeSink::create(new, &renewed)?;
        let len = records.len();
        Ok(Fx::over(Seam::Rerecording {
            from: Mutex::new(ReplaySeam::new(EffectsLog(records))),
            len,
            to,
            live: live_next(header),
        }))
    }

    /// `record`, for the integration tests (plan §1, rule 9): its body is that one call.
    /// # Errors
    /// As `record`.
    #[cfg(any(test, debug_assertions))]
    pub fn record_for_tests(path: &Path, header: &tape::TapeHeader) -> Result<Fx> {
        Fx::record(path, header)
    }

    /// `replay`, for the integration tests (plan §1, rule 9): its body is that one call.
    /// # Errors
    /// As `replay`.
    #[cfg(any(test, debug_assertions))]
    pub fn replay_for_tests(path: &Path) -> Result<(Fx, tape::TapeHeader)> {
        Fx::replay(path)
    }

    /// `rerecord`, for the integration tests (plan §1, rule 9): its body is that one call.
    /// # Errors
    /// As `rerecord`.
    #[cfg(any(test, debug_assertions))]
    pub fn rerecord_for_tests(old: &Path, new: &Path) -> Result<(Fx, tape::TapeHeader)> {
        Fx::rerecord(old, new)
    }

    /// Which world the doors reach.
    #[must_use]
    pub const fn mode(&self) -> Mode {
        match &self.seam {
            Seam::Live(_) => Mode::Live,
            #[cfg(any(test, debug_assertions))]
            Seam::Simulated(_) => Mode::Simulated,
            Seam::Recording { .. } => Mode::Recording,
            Seam::Replaying { .. } => Mode::Replaying,
            Seam::Rerecording { .. } => Mode::Rerecording,
        }
    }

    fn latched(&self) -> Result<()> {
        match &*lock(&self.latch) {
            Some(e) => Err(e.clone()),
            None => Ok(()),
        }
    }

    fn latch(&self, e: Error) -> Error {
        if !matches!(e.sub, Sub::Replay(_)) {
            return e;
        }
        let e = match (&self.seam, &e.sub) {
            (
                Seam::Rerecording { live, .. },
                Sub::Replay(
                    Replay::Exhausted
                    | Replay::KindMismatch
                    | Replay::DoorDiverged
                    | Replay::RequestDiverged
                    | Replay::Unconsumed,
                ),
            )
            | (
                Seam::Replaying { live, .. },
                Sub::Replay(Replay::Exhausted | Replay::KindMismatch | Replay::DoorDiverged),
            ) => e.with_next(live.clone()),
            _ => e,
        };
        let mut latch = lock(&self.latch);
        latch.get_or_insert(e).clone()
    }

    fn replayed<T>(&self, answer: std::result::Result<T, ReplayDivergence>) -> Result<T> {
        answer.map_err(|d| self.latch(diverged(&d)))
    }

    /// Time since this Fx was made.
    /// # Errors
    /// A replay's divergence (`Sub::Replay`), which the Fx latches.
    pub fn mono(&self) -> Result<Duration> {
        let nanos = match &self.seam {
            Seam::Live(seam) => infallible(lock(seam).mono_nanos()),
            #[cfg(any(test, debug_assertions))]
            Seam::Simulated(clock) => return Ok(clock.mono()),
            Seam::Recording { seam, .. } => {
                self.latched()?;
                infallible(lock(seam).mono_nanos())
            }
            Seam::Replaying { seam, .. } => {
                self.latched()?;
                self.replayed(lock(seam).mono_nanos())?
            }
            Seam::Rerecording { from, to, .. } => {
                self.latched()?;
                let nanos = self.replayed(lock(from).mono_nanos())?;
                to.append(&EffectRecord::Mono(nanos));
                nanos
            }
        };
        Ok(Duration::from_nanos(nanos))
    }

    /// Unix seconds: the wall clock's nanoseconds over 10^9, rounded down.
    /// # Errors
    /// As `mono`.
    pub fn wall_secs(&self) -> Result<u64> {
        let nanos = match &self.seam {
            Seam::Live(seam) => infallible(lock(seam).now_nanos()),
            #[cfg(any(test, debug_assertions))]
            Seam::Simulated(clock) => return Ok(clock.wall_secs()),
            Seam::Recording { seam, .. } => {
                self.latched()?;
                infallible(lock(seam).now_nanos())
            }
            Seam::Replaying { seam, .. } => {
                self.latched()?;
                self.replayed(lock(seam).now_nanos())?
            }
            Seam::Rerecording { from, to, .. } => {
                self.latched()?;
                let nanos = self.replayed(lock(from).now_nanos())?;
                to.append(&EffectRecord::Clock(nanos));
                nanos
            }
        };
        Ok(nanos / 1_000_000_000)
    }

    /// Sleep `d`.
    /// # Errors
    /// As `mono`.
    pub fn sleep(&self, d: Duration) -> Result<()> {
        match &self.seam {
            Seam::Live(_) => std::thread::sleep(d),
            #[cfg(any(test, debug_assertions))]
            Seam::Simulated(clock) => clock.sleep(d),
            Seam::Recording { seam, .. } => {
                self.latched()?;
                std::thread::sleep(d);
                let request = sleep_request(d);
                infallible(lock(seam).exchange(DOOR_SLEEP, &request, &mut |_| Vec::new()));
            }
            Seam::Replaying { seam, .. } => {
                self.latched()?;
                let request = sleep_request(d);
                self.replayed(lock(seam).exchange(DOOR_SLEEP, &request, &mut |_| Vec::new()))?;
            }
            Seam::Rerecording { from, to, .. } => {
                self.latched()?;
                let request = sleep_request(d);
                let reply =
                    self.replayed(lock(from).exchange(DOOR_SLEEP, &request, &mut |_| Vec::new()))?;
                to.append(&EffectRecord::Exchange {
                    door: DOOR_SLEEP.to_string(),
                    request,
                    reply,
                });
            }
        }
        Ok(())
    }

    /// One GitHub round trip, its body read whole inside the door under `req.limit`.
    /// # Errors
    /// The outer `Result`: a replay's divergence.
    pub(crate) fn gh(
        &self,
        transport: &dyn Transport,
        endpoints: &Endpoints,
        req: WireRequest<'_>,
    ) -> Result<std::result::Result<WireResponse, WireError>> {
        let keep = |headers| canon::keep(endpoints, headers);
        match &self.seam {
            Seam::Live(_) => Ok(read_whole(transport, req, keep).map(canon::Reply::into_response)),
            #[cfg(any(test, debug_assertions))]
            Seam::Simulated(_) => {
                Ok(read_whole(transport, req, keep).map(canon::Reply::into_response))
            }
            Seam::Recording { seam, sink } => {
                self.latched()?;
                let request = canon::request(&req, endpoints).map_err(|e| self.latch(e))?;
                if tape::redacts(&request) {
                    sink.discard();
                    return Err(self.latch(shaped_request()));
                }
                let answer = read_whole(transport, req, keep);
                let reply = match &answer {
                    Ok(reply) => canon::reply(reply.status, &reply.headers, &reply.body),
                    Err(e) => canon::failure(e),
                };
                if tape::redacts_a_reply(&reply) {
                    sink.discard();
                    return Err(self.latch(shaped_reply()));
                }
                infallible(lock(seam).exchange(DOOR_GH, &request, &mut |_| reply.clone()));
                Ok(answer.map(canon::Reply::into_response))
            }
            Seam::Replaying { seam, .. } => {
                self.latched()?;
                let request = canon::request(&req, endpoints).map_err(|e| self.latch(e))?;
                let reply =
                    self.replayed(lock(seam).exchange(DOOR_GH, &request, &mut |_| Vec::new()))?;
                let answer = canon::decode(&reply).map_err(|e| self.latch(e))?;
                Ok(answer.map(|r| kept_response(endpoints, r)))
            }
            Seam::Rerecording { from, to, .. } => {
                self.latched()?;
                let request = canon::request(&req, endpoints).map_err(|e| self.latch(e))?;
                let reply =
                    self.replayed(lock(from).exchange(DOOR_GH, &request, &mut |_| Vec::new()))?;
                let answer = canon::decode(&reply).map_err(|e| self.latch(e))?;
                if tape::redacts_a_reply(&reply) {
                    return Err(self.latch(shaped_reply()));
                }
                to.append(&EffectRecord::Exchange {
                    door: DOOR_GH.to_string(),
                    request,
                    reply,
                });
                Ok(answer.map(|r| kept_response(endpoints, r)))
            }
        }
    }

    /// `gh`, for the integration tests (plan §1, rule 9): its body is that one call.
    /// # Errors
    /// As `gh`.
    #[cfg(any(test, debug_assertions))]
    pub fn gh_for_tests(
        &self,
        transport: &dyn Transport,
        endpoints: &Endpoints,
        req: WireRequest<'_>,
    ) -> Result<std::result::Result<WireResponse, WireError>> {
        self.gh(transport, endpoints, req)
    }

    /// A streamed round trip (`Github::call_stream`: downloads).
    /// # Errors
    /// Recording, replaying and rerecording: `Replay::StreamNotTaped`, before anything is sent.
    pub(crate) fn gh_stream(
        &self,
        transport: &dyn Transport,
        req: WireRequest<'_>,
    ) -> Result<std::result::Result<WireResponse, WireError>> {
        match &self.seam {
            Seam::Live(_) => {}
            #[cfg(any(test, debug_assertions))]
            Seam::Simulated(_) => {}
            Seam::Recording { .. } | Seam::Replaying { .. } | Seam::Rerecording { .. } => {
                self.latched()?;
                return Err(self.latch(stream_not_taped()));
            }
        }
        Ok(transport.round_trip(req).map(|response| WireResponse {
            headers: canon::cut(response.headers),
            ..response
        }))
    }

    /// `gh_stream`, for the integration tests (plan §1, rule 9): its body is that one call.
    /// # Errors
    /// As `gh_stream`.
    #[cfg(any(test, debug_assertions))]
    pub fn gh_stream_for_tests(
        &self,
        transport: &dyn Transport,
        req: WireRequest<'_>,
    ) -> Result<std::result::Result<WireResponse, WireError>> {
        self.gh_stream(transport, req)
    }

    /// One stdout write, as rendered, before the terminal filter.
    /// # Errors
    /// A replay's divergence, latched.
    pub fn emit_stdout(&self, bytes: &[u8]) -> Result<()> {
        match &self.seam {
            Seam::Live(_) => {}
            #[cfg(any(test, debug_assertions))]
            Seam::Simulated(_) => {}
            Seam::Recording { seam, .. } => {
                self.latched()?;
                infallible(lock(seam).emit(DOOR_STDOUT, bytes));
            }
            Seam::Replaying { seam, .. } => {
                self.latched()?;
                self.replayed(lock(seam).emit(DOOR_STDOUT, &tape::shaped(bytes)))?;
            }
            Seam::Rerecording { to, .. } => {
                self.latched()?;
                to.append(&EffectRecord::Emit {
                    door: DOOR_STDOUT.to_string(),
                    bytes: bytes.to_vec(),
                });
            }
        }
        Ok(())
    }

    /// The run's end.
    /// # Errors
    /// Under a tape: the latched error, or `tape::TapeSink::finish`'s; replaying, a divergence
    /// at the exit; replaying and rerecording, `Replay::Unconsumed`, why
    /// `the tape holds {n} more records after the run's exit`.
    pub fn finish(&self, exit: u8) -> Result<()> {
        let code = exit.to_string();
        match &self.seam {
            Seam::Live(_) => Ok(()),
            #[cfg(any(test, debug_assertions))]
            Seam::Simulated(_) => Ok(()),
            Seam::Recording { seam, sink } => {
                if sink.discarded() {
                    return self.latched();
                }
                infallible(lock(seam).emit(DOOR_EXIT, code.as_bytes()));
                sink.finish()
            }
            Seam::Replaying { seam, len, .. } => {
                self.latched()?;
                let mut seam = lock(seam);
                self.replayed(seam.emit(DOOR_EXIT, code.as_bytes()))?;
                seam.finish().map_err(|d| self.latch(ended(&d, *len)))
            }
            Seam::Rerecording { from, len, to, .. } => {
                let replayed = self
                    .latched()
                    .and_then(|()| lock(from).finish().map_err(|d| self.latch(ended(&d, *len))));
                if replayed.is_err() {
                    to.discard();
                    return replayed;
                }
                to.append(&EffectRecord::Emit {
                    door: DOOR_EXIT.to_string(),
                    bytes: code.into_bytes(),
                });
                to.finish()
            }
        }
    }

    /// Every sleep so far, in order: simulated only, empty in every other mode.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn slept(&self) -> Vec<Duration> {
        match &self.seam {
            Seam::Simulated(clock) => clock.slept(),
            _ => Vec::new(),
        }
    }

    /// A routing decision. Live and simulated: `decide()`. Recording a `mode credentialed`
    /// tape: `decide()`, taped as an exchange at `DOOR_ROUTE` (`canon::route_request`,
    /// `canon::route_reply`). Replaying one: the tape's decision for the same inputs, and
    /// `decide` is never called; another input is `replay.request`. Rerecording: as
    /// replaying, taped again. Under a `mode anonymous` tape, in every mode: `decide()`,
    /// untaped, over the empty router M1's tapes run with, so every M1 tape replays unchanged.
    ///
    /// # Errors
    ///
    /// A replay's divergence, latched.
    pub(crate) fn route(
        &self,
        req: &RouteRequest,
        decide: &mut dyn FnMut() -> Decision,
    ) -> Result<Decision> {
        let _ = req;
        Ok(decide())
    }

    /// One journal record's body. Live and simulated: `Ok(None)`, and the journal writes its
    /// file. Under any tape: an emit at `DOOR_JOURNAL`, compared on replay as stdout is, and
    /// `Ok(Some(step))` from the tape's own counter of journal emits, from 0.
    ///
    /// # Errors
    ///
    /// A replay's divergence, latched.
    pub(crate) fn emit_journal(&self, body: &[u8]) -> Result<Option<u64>> {
        let _ = body;
        match &self.seam {
            Seam::Live(_) => Ok(None),
            #[cfg(any(test, debug_assertions))]
            Seam::Simulated(_) => Ok(None),
            Seam::Recording { .. } | Seam::Replaying { .. } | Seam::Rerecording { .. } => {
                Err(Error::not_yet("the journal door's tape arms", "WP10"))
            }
        }
    }

    /// One fabric post. Live and simulated: `send()`, its answer returned. Under any tape: an
    /// emit at `DOOR_FABRIC` of `line`, compared on replay; `send` is never called, and the
    /// answer is `Posted::Skipped("taped")`.
    ///
    /// # Errors
    ///
    /// A replay's divergence, latched.
    pub(crate) fn emit_fabric(
        &self,
        line: &str,
        send: &mut dyn FnMut() -> crate::notify::Posted,
    ) -> Result<crate::notify::Posted> {
        let _ = line;
        match &self.seam {
            Seam::Live(_) => Ok(send()),
            #[cfg(any(test, debug_assertions))]
            Seam::Simulated(_) => Ok(send()),
            Seam::Recording { .. } | Seam::Replaying { .. } | Seam::Rerecording { .. } => {
                Err(Error::not_yet("the fabric door's tape arms", "WP10"))
            }
        }
    }

    /// One child process (`gh auth token`). Live: `child::run(spec)`. Simulated: first the
    /// live arm's refusal of a relative program (`child::check`, so a run the live seam would
    /// refuse fails its simulated test too), then the next scripted child, whose program and
    /// args must equal `spec`'s (a mismatch panics in the test that scripted it); an empty
    /// scripted queue is could_not_run, why `{program} could not start: no scripted child`, as
    /// a missing program is live. Under any tape: `Replay::StreamNotTaped`, before anything
    /// runs.
    ///
    /// # Errors
    ///
    /// As above; the run's could_not_run.
    pub(crate) fn child(&self, spec: &child::ChildSpec) -> Result<child::ChildOut> {
        match &self.seam {
            Seam::Live(_) => child::run(spec),
            #[cfg(any(test, debug_assertions))]
            Seam::Simulated(clock) => {
                child::check(spec)?;
                clock.log(spec);
                let Some(scripted) = clock.next_child() else {
                    return Err(Error::could_not_run(format!(
                        "{} could not start: no scripted child",
                        spec.program.display()
                    ))
                    .with_sub(Sub::Local));
                };
                let args: Vec<String> = spec
                    .args
                    .iter()
                    .map(|arg| arg.to_string_lossy().into_owned())
                    .collect();
                assert_eq!(
                    (&scripted.program, &scripted.args),
                    (&spec.program, &args),
                    "the scripted child is not the one this run asked for"
                );
                Ok(scripted.out)
            }
            Seam::Recording { .. } | Seam::Replaying { .. } | Seam::Rerecording { .. } => {
                self.latched()?;
                Err(self.latch(child_not_taped()))
            }
        }
    }

    /// One git run (`Github::run_git`). Live and simulated: `child::run(spec)`, the real
    /// `git` (a simulated test runs the host's git against agh-fake or a local repository;
    /// simulated, `child::check` passes before the spec is logged, as in `child`).
    /// Recording: run, and taped as an exchange at `DOOR_GIT` (`canon::git_request`,
    /// `canon::git_reply`). Replaying: the tape's answer, and nothing runs. Rerecording: as
    /// replaying, taped again.
    ///
    /// # Errors
    ///
    /// A replay's divergence, latched; the run's could_not_run.
    pub(crate) fn git(&self, spec: &child::ChildSpec, canonical: &[u8]) -> Result<child::ChildOut> {
        let _ = canonical;
        match &self.seam {
            Seam::Live(_) => child::run(spec),
            #[cfg(any(test, debug_assertions))]
            Seam::Simulated(clock) => {
                child::check(spec)?;
                clock.log(spec);
                child::run(spec)
            }
            Seam::Recording { .. } | Seam::Replaying { .. } | Seam::Rerecording { .. } => {
                Err(Error::not_yet("the git door's tape arms", "WP10"))
            }
        }
    }

    /// Tests: the same simulated `Fx`, answering `child` from `children` in order.
    ///
    /// # Panics
    ///
    /// When the `Fx` is not simulated: a live `Fx` would run the real child (the host's
    /// `gh auth token`) in place of the scripted one, and a taped one answers every child
    /// `Replay::StreamNotTaped` and latches. A test built either way fails here, before it
    /// reads a real credential or poisons a tape.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn with_children(self, children: Vec<child::ScriptedChild>) -> Fx {
        let Seam::Simulated(clock) = &self.seam else {
            panic!(
                "Fx::with_children scripts only a simulated Fx, not {:?}",
                self.mode()
            );
        };
        clock.script(children);
        self
    }

    /// Tests: every spec `child` or `git` accepted (one `child::check` passed) under a simulated
    /// `Fx`, in call order, so a test of the git runner reads the environment and directories
    /// its children got.
    /// Empty in every other mode.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    pub fn children_run(&self) -> Vec<child::ChildSpec> {
        match &self.seam {
            Seam::Simulated(clock) => clock.children_run(),
            _ => Vec::new(),
        }
    }
}

impl std::fmt::Debug for Fx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Fx({:?})", self.mode())
    }
}

/// Names the frozen crate-private doors whose first callers later packages write (WP3's
/// route, WP4's journal, WP7's git, WP8's child, WP9's fabric), so the plain library's
/// `dead_code` lint passes at every commit until then (plan §1, rule 14). It runs nothing
/// and nothing calls it. No package edits it after WP0c; WP14 deletes it.
#[doc(hidden)]
pub fn frozen_roots() {
    let _ = Fx::route;
    let _ = Fx::emit_journal;
    let _ = Fx::emit_fabric;
    let _ = Fx::child;
    let _ = Fx::git;
}
